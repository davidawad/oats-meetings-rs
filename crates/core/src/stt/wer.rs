//! Word error rate: word-level Levenshtein distance over the reference length.

/// Lower-case words; everything except letters, digits and apostrophes separates words.
pub fn words(s: &str) -> Vec<String> {
    s.to_lowercase()
        .replace(['\u{2019}', '\u{2018}'], "'")
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn wer(reference: &str, hypothesis: &str) -> f64 {
    let r = words(reference);
    let h = words(hypothesis);
    if r.is_empty() {
        return if h.is_empty() { 0.0 } else { 1.0 };
    }
    let mut prev: Vec<usize> = (0..=h.len()).collect();
    for (i, rw) in r.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, hw) in h.iter().enumerate() {
            let sub = prev[j] + usize::from(rw != hw);
            cur.push(sub.min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[h.len()] as f64 / r.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_values() {
        assert_eq!(wer("a b c d", "a b c d"), 0.0);
        assert_eq!(wer("a b c d", "a x c d"), 0.25);
        assert_eq!(wer("a b c d", "a b c"), 0.25);
        assert_eq!(wer("a b", "a b c d"), 1.0);
        assert_eq!(wer("", ""), 0.0);
        assert_eq!(wer("Let's go.", "lets go"), 0.5);
        assert_eq!(wer("It's  FINE!", "it's fine"), 0.0);
    }

    #[test]
    fn normalisation() {
        assert_eq!(words("Hello, World! It\u{2019}s 5pm."), ["hello", "world", "it's", "5pm"]);
    }
}
