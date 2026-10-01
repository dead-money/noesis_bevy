//! Minimal `#ifdef` / `#ifndef` / `#endif` preprocessor for WGSL.
//!
//! WGSL has no preprocessor. `noesis.wgsl` follows Noesis's GL shader
//! convention instead: one source with `#ifdef`-gated branches, stripped
//! against a define set before it reaches `naga`.
//!
//! Supported subset:
//! - A directive is the first non-whitespace text on its own line, followed
//!   by one space and the name. Surrounding whitespace on the name is ignored.
//! - `#ifdef NAME`, `#ifndef NAME`, and `#endif`, nested to any depth. No
//!   `#else`, `#elif`, or `#define`.
//! - Directive lines and every line inside an inactive branch are dropped.

use std::collections::HashSet;
use std::hash::BuildHasher;

/// Returns `source` with inactive branches removed for the given `defines`.
/// Every output line ends in `\n`.
///
/// # Panics
///
/// Panics on an `#endif` with no open directive, or an `#ifdef`/`#ifndef`
/// still open at end of source.
#[must_use]
pub fn preprocess<S: BuildHasher>(source: &str, defines: &HashSet<&'static str, S>) -> String {
    let mut out = String::with_capacity(source.len());
    let mut stack: Vec<bool> = vec![true];

    for (lineno, line) in source.lines().enumerate() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("#ifdef ") {
            let name = rest.trim();
            let active = *stack.last().expect("preproc stack underflow") && defines.contains(name);
            stack.push(active);
        } else if let Some(rest) = trimmed.strip_prefix("#ifndef ") {
            let name = rest.trim();
            let active = *stack.last().expect("preproc stack underflow") && !defines.contains(name);
            stack.push(active);
        } else if trimmed.starts_with("#endif") {
            assert!(
                stack.len() > 1,
                "unmatched #endif at line {} of WGSL source",
                lineno + 1
            );
            stack.pop();
        } else if *stack.last().expect("preproc stack underflow") {
            out.push_str(line);
            out.push('\n');
        }
    }
    assert_eq!(
        stack.len(),
        1,
        "unterminated #ifdef/#ifndef in WGSL source ({} open at EOF)",
        stack.len() - 1
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defs(names: &[&'static str]) -> HashSet<&'static str> {
        names.iter().copied().collect()
    }

    #[test]
    fn passes_through_when_no_directives() {
        let out = preprocess("foo\nbar\n", &defs(&[]));
        assert_eq!(out, "foo\nbar\n");
    }

    #[test]
    fn ifdef_kept_when_defined() {
        let out = preprocess("a\n#ifdef X\nb\n#endif\nc\n", &defs(&["X"]));
        assert_eq!(out, "a\nb\nc\n");
    }

    #[test]
    fn ifdef_dropped_when_undefined() {
        let out = preprocess("a\n#ifdef X\nb\n#endif\nc\n", &defs(&[]));
        assert_eq!(out, "a\nc\n");
    }

    #[test]
    fn ifndef_complements_ifdef() {
        let out = preprocess("#ifndef X\nb\n#endif\n", &defs(&[]));
        assert_eq!(out, "b\n");
        let out = preprocess("#ifndef X\nb\n#endif\n", &defs(&["X"]));
        assert_eq!(out, "");
    }

    #[test]
    fn nested_ifdef_works() {
        let src = "#ifdef A\nouter\n#ifdef B\ninner\n#endif\nafter\n#endif\n";
        assert_eq!(preprocess(src, &defs(&["A", "B"])), "outer\ninner\nafter\n");
        assert_eq!(preprocess(src, &defs(&["A"])), "outer\nafter\n");
        assert_eq!(preprocess(src, &defs(&["B"])), "");
    }
}
