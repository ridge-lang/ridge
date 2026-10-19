//! Test discovery and signature validation shared by the runner and editor.
//!
//! This policy reads declared syntax, never inferred types or runtime state.

use ridge_ast::{Attribute, Capability, FnDecl, PrimitiveType, Type, Visibility};

/// Whether a discovered test may execute under the test runner contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestClassification {
    /// Canonical `Result Unit Text` test.
    Canonical,
    /// Accepted compatibility test returning `Bool`.
    BoolDeprecated,
    /// Signature validation prevents execution.
    Rejected(TestRejection),
}

/// A signature error that prevents a test from executing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestRejection {
    /// A test must have zero parameters.
    ArityInvalid,
    /// The test runner does not permit the `ffi` capability.
    CapabilityForbidden,
    /// The declared result is unsupported.
    ReturnTypeInvalid,
    /// The test has no declared return type.
    ReturnTypeMissing,
}

impl TestRejection {
    /// Stable diagnostic identity, shared by CLI and editor.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::ArityInvalid => "C301",
            Self::CapabilityForbidden => "C302",
            Self::ReturnTypeInvalid => "C305",
            Self::ReturnTypeMissing => "C306",
        }
    }

    /// Explain the rejection and its remedy, without repeating the code.
    #[must_use]
    pub fn message(self, name: &str) -> impl std::fmt::Display + '_ {
        RejectionMessage { reason: self, name }
    }
}

struct RejectionMessage<'a> {
    reason: TestRejection,
    name: &'a str,
}

impl std::fmt::Display for RejectionMessage<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = self.name;
        match self.reason {
            TestRejection::ArityInvalid => write!(
                f, "TestArityInvalid: '{name}' must have zero parameters; test functions cannot take arguments"
            ),
            TestRejection::CapabilityForbidden => write!(
                f, "TestCapabilityForbidden: '{name}' declares the 'ffi' capability; ffi tests are not permitted in ridge test"
            ),
            TestRejection::ReturnTypeInvalid => write!(
                f, "TestReturnTypeInvalid: '{name}' returns a type the test runner cannot use; a test must return Result Unit Text"
            ),
            TestRejection::ReturnTypeMissing => write!(
                f, "TestReturnTypeMissing: '{name}' declares no return type; add `-> Result Unit Text`"
            ),
        }
    }
}

/// One discovered test, borrowing its display name from the declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TestFunction<'a> {
    /// Attribute display name, or the function name for legacy discovery.
    pub display_name: &'a str,
    /// Whether discovery used the deprecated public `test_` prefix.
    pub legacy_prefix: bool,
    /// The result of validating the declared signature.
    pub classification: TestClassification,
}

/// Discover and classify a function once, with attribute discovery taking priority.
///
/// Validation keeps the runner's precedence: parameters, `ffi`, declared return.
/// A private prefix-only function is not a test. `Bool` stays executable.
#[must_use]
pub fn discover_test(function: &FnDecl) -> Option<TestFunction<'_>> {
    let attribute_name = function.attrs.first().map(|attr| match attr {
        Attribute::Test { name, .. } => name.as_str(),
    });
    let (display_name, legacy_prefix) = if let Some(name) = attribute_name {
        (name, false)
    } else if function.vis == Visibility::Pub && function.name.text.starts_with("test_") {
        (function.name.text.as_str(), true)
    } else {
        return None;
    };

    let classification = if !function.params.is_empty() {
        TestClassification::Rejected(TestRejection::ArityInvalid)
    } else if function.caps.contains(&Capability::Ffi) {
        TestClassification::Rejected(TestRejection::CapabilityForbidden)
    } else {
        classify_return(function.ret.as_ref())
    };
    Some(TestFunction {
        display_name,
        legacy_prefix,
        classification,
    })
}

fn classify_return(ret: Option<&Type>) -> TestClassification {
    match ret {
        None => TestClassification::Rejected(TestRejection::ReturnTypeMissing),
        Some(Type::Primitive {
            name: PrimitiveType::Bool,
            ..
        }) => TestClassification::BoolDeprecated,
        Some(Type::App { head, args, .. })
            if head.text == "Result"
                && matches!(
                    args.as_slice(),
                    [
                        Type::Primitive {
                            name: PrimitiveType::Unit,
                            ..
                        },
                        Type::Primitive {
                            name: PrimitiveType::Text,
                            ..
                        },
                    ]
                ) =>
        {
            TestClassification::Canonical
        }
        Some(_) => TestClassification::Rejected(TestRejection::ReturnTypeInvalid),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn function(source: &str) -> FnDecl {
        let parsed = ridge_parser::parse_source(source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        assert!(parsed.lex_errors.is_empty(), "{:?}", parsed.lex_errors);
        let ridge_ast::Item::Fn(function) = parsed.module.items.into_iter().next().unwrap() else {
            panic!("expected a function");
        };
        function
    }

    #[test]
    fn discovery_preserves_attributes_prefixes_and_declared_results() {
        for (source, expected) in [
            (
                "@test \"named\"\nfn test_body () -> Result Unit Text = Ok ()\n",
                Some(("named", false, TestClassification::Canonical)),
            ),
            (
                "pub fn test_body () -> Bool = true\n",
                Some(("test_body", true, TestClassification::BoolDeprecated)),
            ),
            ("fn test_body () -> Int = 1\n", None),
            ("pub fn ordinary () -> Int = 1\n", None),
        ] {
            let declaration = function(source);
            assert_eq!(
                discover_test(&declaration).map(|test| (
                    test.display_name,
                    test.legacy_prefix,
                    test.classification
                )),
                expected,
                "{source}"
            );
        }
    }

    #[test]
    fn rejection_precedence_and_declared_return_contract_stay_intact() {
        for (signature, reason) in [
            (
                "ffi invalid (x: Int) -> Int = 1",
                TestRejection::ArityInvalid,
            ),
            (
                "ffi invalid () -> Int = 1",
                TestRejection::CapabilityForbidden,
            ),
            ("invalid () -> Int = 1", TestRejection::ReturnTypeInvalid),
            ("invalid = Ok ()", TestRejection::ReturnTypeMissing),
            (
                "invalid () -> Result Text Unit = Err ()",
                TestRejection::ReturnTypeInvalid,
            ),
            (
                "invalid () -> Result Unit Bool = Ok ()",
                TestRejection::ReturnTypeInvalid,
            ),
        ] {
            for declaration in [
                format!("@test \"invalid\"\nfn {signature}\n"),
                format!(
                    "pub fn {}\n",
                    signature.replacen("invalid", "test_invalid", 1)
                ),
            ] {
                let function = function(&declaration);
                assert_eq!(
                    discover_test(&function).unwrap().classification,
                    TestClassification::Rejected(reason),
                    "{declaration}"
                );
            }
        }
    }
}
