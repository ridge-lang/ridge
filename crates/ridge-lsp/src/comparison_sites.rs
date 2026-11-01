//! Source tokens for checked comparisons, including the separate SQL domain.

use ridge_ast::visit::{walk_expr, Visit};
use ridge_ast::{BinOp, Expr};
use ridge_lexer::Span;
use ridge_resolve::imports::Binding;

#[derive(Debug)]
pub struct ComparisonSite {
    pub span: Span,
    pub binding: Option<Binding>,
}

impl ComparisonSite {
    pub fn hover(&self) -> (String, Span) {
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
