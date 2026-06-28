//! Tiny GLSL-style `#ifdef` / `#ifndef` / `#endif` preprocessor for WGSL.
//!
//! WGSL has no native preprocessor. We borrow Noesis's GL-shader convention
//! of one source file with `#ifdef`-gated branches, run a stripping pass with
//! a known feature set, and feed the result to `naga`. This is enough for the
//! `noesis.wgsl` template; if the shader matrix grows complex enough to want
//! `#elif` / `#define` value substitution we'd swap to `naga_oil`.
//!
//! Conventions:
//! - Directive is the first non-whitespace text on its own line.
//! - Supported: `#ifdef NAME`, `#ifndef NAME`, `#endif`. Nesting is fine.
//! - Anything between an inactive `#ifdef`/`#ifndef` and its matching `#endif`
//!   is dropped. Trailing whitespace on the directive is ignored.
//! - Unmatched `#endif` panics — caller bug.

use std::collections::HashSet;
use std::hash::BuildHasher;

/// Strip `#ifdef`/`#ifndef`/`#endif` branches from WGSL source according to
/// `defines`. See module docs for the supported subset.
///
/// # Panics
///
/// Panics on a malformed input: an `#endif` without a matching open
/// directive, or an unterminated `#ifdef`/`#ifndef` at end of source. Both
/// indicate a bug in the embedded shader template, not user input.
#[must_use]
pub fn preprocess<S: BuildHasher>(source: &str, defines: &HashSet<&'static str, S>) -> String {
    let mut out = String::with_capacity(source.len());
    // Stack of "is this branch currently emitting?" flags; the outer scope is
    // always emitting.
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
