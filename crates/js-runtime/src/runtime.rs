//! JavaScript runtime based on `boa_engine`.
//!
//! M3.1 scope: a thin wrapper that lets us eval JS code and read the
//! result. The DOM bridge (document.getElementById, createElement,
//! ...) lands in M3.2+.

use boa_engine::{Context, JsValue, Source};

/// A owned JavaScript execution context. Cheap to drop; non-Clone
/// because the underlying boa `Context` is single-threaded.
pub struct JsRuntime {
    ctx: Context,
}

impl Default for JsRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl JsRuntime {
    /// Construct a fresh runtime with default globals.
    #[must_use]
    pub fn new() -> Self {
        Self {
            ctx: Context::default(),
        }
    }

    /// Evaluate a JS source string and return the resulting value
    /// formatted as a string (matching `console.log` / `String()`
    /// behavior).
    ///
    /// # Errors
    /// Returns an error string if the JS source fails to parse or
    /// the evaluation throws.
    pub fn eval(&mut self, code: &str) -> Result<String, String> {
        let bytes = Source::from_bytes(code);
        let result: JsValue = self
            .ctx
            .eval(bytes)
            .map_err(|e| format!("js eval error: {e}"))?;
        Ok(result.display().to_string())
    }

    /// Evaluate code but discard the result. Useful for `<script>`
    /// bodies that don't produce a value we care about.
    ///
    /// # Errors
    /// See [`JsRuntime::eval`].
    pub fn execute(&mut self, code: &str) -> Result<(), String> {
        let _ = self.eval(code)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping() {
        assert_eq!(env!("CARGO_PKG_NAME"), "browser-js-runtime");
    }

    #[test]
    fn eval_arithmetic() {
        let mut rt = JsRuntime::new();
        let result = rt.eval("1 + 1").expect("should succeed");
        assert_eq!(result, "2");
    }

    #[test]
    fn eval_string_concat() {
        let mut rt = JsRuntime::new();
        let result = rt.eval("'hello' + ' ' + 'world'").expect("should succeed");
        // boa's display() adds quotes for string values.
        assert_eq!(result, "\"hello world\"");
    }

    #[test]
    fn eval_assignment_returns_value() {
        let mut rt = JsRuntime::new();
        let result = rt.eval("var x = 42; x * 2").expect("should succeed");
        assert_eq!(result, "84");
    }

    #[test]
    fn eval_persistent_state_across_calls() {
        let mut rt = JsRuntime::new();
        rt.eval("var counter = 10").unwrap();
        rt.eval("counter += 5").unwrap();
        let result = rt.eval("counter").unwrap();
        assert_eq!(result, "15");
    }

    #[test]
    fn eval_returns_error_for_syntax_error() {
        let mut rt = JsRuntime::new();
        let result = rt.eval("var = broken");
        assert!(result.is_err(), "expected error for syntax error");
    }

    #[test]
    fn eval_returns_error_for_thrown_exception() {
        let mut rt = JsRuntime::new();
        let result = rt.eval("throw new Error('boom')");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("boom"));
    }

    #[test]
    fn eval_template_literals() {
        let mut rt = JsRuntime::new();
        let result = rt.eval("const n = 7; `n=${n}`").unwrap();
        assert_eq!(result, "\"n=7\"");
    }

    #[test]
    fn eval_array_and_object_literals() {
        let mut rt = JsRuntime::new();
        let arr = rt.eval("[1, 2, 3].length").unwrap();
        assert_eq!(arr, "3");
        let obj = rt.eval("({a: 1, b: 2}).a + ({a: 1, b: 2}).b").unwrap();
        assert_eq!(obj, "3");
    }

    #[test]
    fn execute_discards_result() {
        let mut rt = JsRuntime::new();
        rt.execute("var z = 100").unwrap();
        let v = rt.eval("z").unwrap();
        assert_eq!(v, "100");
    }
}
