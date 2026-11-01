//! Source tokens for checked comparisons, including the separate SQL domain.

use ridge_ast::visit::{walk_expr, Visit};
use ridge_ast::{BinOp, Expr};
use ridge_lexer::Span;
use ridge_resolve::imports::Binding;

#[derive(Debug)]
pub struct ComparisonSite {
    pub span: Span,
    pub binding: Option<Binding>,
    pub is_pattern: bool,
    pub always_matches: bool,
}

impl ComparisonSite {
    pub fn hover(&self) -> (String, Span) {
        if self.is_pattern {
            let mut text = "Typed pattern predicate: `value is pattern -> Bool`. Evaluates the subject once and recognizes its structure independently of `Eq`. A bare constructor ignores its payload. Does not bind names or narrow types; use `match` to extract values.".to_owned();
            if self.always_matches {
                text.push_str(" The wildcard pattern always succeeds; the subject still executes.");
            }
            return (text, self.span);
        }
        let description = match &self.binding {
            Some(Binding::ClassMethod { class_name, method }) => format!(
                "Comparison through `{class_name}.{method}`. Both operands must have the same type and satisfy `{class_name}`."
            ),
            _ => "SQL comparison inside a quotation. Uses database comparison and NULL semantics; does not require a Ridge Eq or Ord instance.".to_owned(),
        };
        (description, self.span)
    }
}

pub fn collect(ast: &ridge_ast::Module, source: &str, quoted: Vec<Span>) -> Vec<ComparisonSite> {
    struct Collector {
        tokens: Vec<(ridge_lexer::Token, Span)>,
        quoted: Vec<Span>,
        sites: Vec<ComparisonSite>,
    }
    impl<'ast> Visit<'ast> for Collector {
        fn visit_expr(&mut self, expr: &'ast Expr) {
            if let Expr::Is {
                subject, pattern, ..
            } = expr
            {
                let start = self
                    .tokens
                    .partition_point(|(_, span)| span.start < subject.span().end);
                if let Some((ridge_lexer::Token::KwIs, span)) = self
                    .tokens
                    .get(start)
                    .filter(|(_, span)| span.end <= pattern.span().start)
                {
                    self.sites.push(ComparisonSite {
                        span: *span,
                        binding: None,
                        is_pattern: true,
                        always_matches: wildcard_pattern(pattern),
                    });
                }
            }
            if let Expr::Binary { op, lhs, rhs, span } = expr {
                let contract = match op {
                    BinOp::Eq | BinOp::Ne => Some(("Eq", "eq")),
                    BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => Some(("Ord", "compare")),
                    _ => None,
                };
                if let Some((class, method)) = contract {
                    let start = self
                        .tokens
                        .partition_point(|(_, s)| s.start < lhs.span().end);
                    if let Some((_, token)) = self
                        .tokens
                        .get(start)
                        .filter(|(_, s)| s.start < rhs.span().start && s.end <= rhs.span().start)
                    {
                        let sql = self
                            .quoted
                            .iter()
                            .any(|q| q.start <= span.start && span.end <= q.end);
                        self.sites.push(ComparisonSite {
                            is_pattern: false,
                            always_matches: false,
                            span: *token,
                            binding: (!sql).then(|| Binding::ClassMethod {
                                class_name: class.to_owned(),
                                method: method.to_owned(),
                            }),
                        });
                    }
                }
            }
            walk_expr(self, expr);
        }
    }
    let mut collector = Collector {
        tokens: ridge_lexer::tokenize(source)
            .tokens
            .into_iter()
            .filter(|(t, _)| {
                matches!(
                    t,
                    ridge_lexer::Token::EqEq
                        | ridge_lexer::Token::KwIs
                        | ridge_lexer::Token::BangEq
                        | ridge_lexer::Token::Lt
                        | ridge_lexer::Token::Gt
                        | ridge_lexer::Token::Le
                        | ridge_lexer::Token::Ge
                )
            })
            .collect(),
        quoted,
        sites: Vec::new(),
    };
    collector.visit_module(ast);
    collector.sites
}

fn wildcard_pattern(pattern: &ridge_ast::Pattern) -> bool {
    match pattern {
        ridge_ast::Pattern::Wildcard { .. } => true,
        ridge_ast::Pattern::Paren { inner, .. } => wildcard_pattern(inner),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn comments_and_string_contents_are_not_operator_sites() {
        let source =
            "fn check left right = (left -- == != <\n    >= right)\nfn text () = \"== != < >=\"\n";
        let parsed = ridge_parser::parse_source(source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let sites = super::collect(&parsed.module, source, Vec::new());
        assert_eq!(sites.len(), 1);
        let site = &sites[0];
        assert_eq!(
            &source[site.span.start as usize..site.span.end as usize],
            ">="
        );
    }
}
