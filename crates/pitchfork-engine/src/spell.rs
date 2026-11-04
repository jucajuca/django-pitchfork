//! FST-based spelling suggestions (phase 5).
//!
//! Tantivy has no built-in spell-check — this is custom work on top of the
//! term dictionary: a Levenshtein automaton (distance 2) streams candidate
//! terms from the content field's dictionary, and the best candidate is
//! ranked by edit distance then document frequency (whoosh's behavior).

use crate::{CompiledIndex, EngineError, Result};
use std::collections::HashMap;
use tantivy::collector::Count;
use tantivy::query::{FuzzyTermQuery, TermQuery};
use tantivy::schema::{IndexRecordOption, TantivyDocument};
use tantivy::{Searcher, Term};

/// Maximum number of candidate terms to rank per misspelled word.
const MAX_CANDIDATES: usize = 20;

/// Suggest a corrected query string: each word with length >= 3 that is not
/// in the dictionary is replaced by the closest dictionary term.
pub(crate) fn spelling_suggestion(
    searcher: &Searcher,
    compiled: &CompiledIndex,
    query: &str,
) -> Result<Option<String>> {
    let field_name = compiled.document_field_name().to_string();
    let field = compiled
        .tantivy_schema
        .get_field(&field_name)
        .map_err(|_| EngineError::Schema(format!("missing document field '{field_name}'")))?;

    let words: Vec<String> = query
        .split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect();

    if words.is_empty() {
        return Ok(None);
    }

    // All dictionary terms of the content field, across segments.
    let mut dictionary: std::collections::HashSet<String> = std::collections::HashSet::new();
    for seg in searcher.segment_readers() {
        let Ok(inv) = seg.inverted_index(field) else {
            continue;
        };
        let Ok(mut stream) = inv.terms().stream() else {
            continue;
        };
        while stream.advance() {
            if let Ok(term) = std::str::from_utf8(stream.key()) {
                dictionary.insert(term.to_string());
            }
        }
    }

    let mut corrected: Vec<String> = Vec::with_capacity(words.len());
    let mut changed = false;

    for word in &words {
        if dictionary.contains(word) || word.chars().count() < 3 {
            corrected.push(word.clone());
            continue;
        }

        match best_candidate(searcher, compiled, field, word)? {
            Some(candidate) => {
                changed = true;
                corrected.push(candidate);
            }
            None => corrected.push(word.clone()),
        }
    }

    if !changed {
        return Ok(None);
    }
    Ok(Some(corrected.join(" ")))
}

/// Find the dictionary term closest to `word` within edit distance 2,
/// ranked by (edit distance, then document frequency, then lexicographic).
fn best_candidate(
    searcher: &Searcher,
    compiled: &CompiledIndex,
    field: tantivy::schema::Field,
    word: &str,
) -> Result<Option<String>> {
    let term = Term::from_field_text(field, word);

    // Candidates from every segment's term dictionary (deduped).
    let mut candidates: Vec<String> = Vec::new();
    for seg in searcher.segment_readers() {
        let inv = seg.inverted_index(field).map_err(EngineError::from)?;
        // A fresh automaton per segment: DfaWrapper is not Clone.
        let automaton = FuzzyTermQuery::new(term.clone(), 2, true)
            .automaton()
            .map_err(EngineError::from)?;
        let mut stream = inv
            .terms()
            .search(automaton)
            .into_stream()
            .map_err(EngineError::from)?;
        while stream.advance() {
            if let Ok(candidate) = std::str::from_utf8(stream.key()) {
                if !candidates.iter().any(|c| c == candidate) {
                    candidates.push(candidate.to_string());
                }
            }
            if candidates.len() >= MAX_CANDIDATES {
                break;
            }
        }
    }

    if candidates.is_empty() {
        return Ok(None);
    }

    // Rank by edit distance, then document frequency.
    let mut ranked: Vec<(u32, i64, String)> = candidates
        .into_iter()
        .map(|candidate| {
            let distance = damerau_levenshtein(word, &candidate) as u32;
            let doc_freq = term_doc_freq(searcher, compiled, field, &candidate);
            (distance, -doc_freq, candidate)
        })
        .collect();
    ranked.sort();

    Ok(ranked.first().map(|(_, _, candidate)| candidate.clone()))
}

/// Summed document frequency of a text term across all segments.
fn term_doc_freq(
    searcher: &Searcher,
    compiled: &CompiledIndex,
    field: tantivy::schema::Field,
    term_text: &str,
) -> i64 {
    let _ = compiled;
    let tq = TermQuery::new(
        Term::from_field_text(field, term_text),
        IndexRecordOption::Basic,
    );
    searcher.search(&tq, &Count).unwrap_or(0) as i64
}

/// Damerau-Levenshtein distance (optimal string alignment).
#[allow(clippy::needless_range_loop)]
pub fn damerau_levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (n, m) = (a.len(), b.len());
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }

    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for j in 0..=m {
        d[0][j] = j;
    }

    for i in 1..=n {
        for j in 1..=m {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            d[i][j] = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[n][m]
}

/// Convenience: extract the stored text of a document (unused today but kept
/// for the highlight path's tests).
#[allow(dead_code)]
pub(crate) fn first_stored_text(
    doc: &TantivyDocument,
    field: tantivy::schema::Field,
) -> Option<String> {
    use tantivy::schema::OwnedValue;
    doc.get_all(field).find_map(|v| match OwnedValue::from(v) {
        OwnedValue::Str(s) => Some(s),
        _ => None,
    })
}

/// Track which words map to which candidates (used by tests).
#[allow(dead_code)]
pub(crate) type CandidateMap = HashMap<String, Vec<String>>;
