use miette::Diagnostic;
use nom_supreme::error::{
    BaseErrorKind, ErrorTree, GenericErrorTree, StackContext,
};
use thiserror::Error;

use crate::parser::Span;

pub type ParseError<'a> = ErrorTree<Span<'a>>;

#[derive(Error, Debug, Diagnostic)]
#[error("Parse Error")]
pub struct FormattedError<'b> {
    #[source_code]
    src: &'b str,
    #[label("{kind}")]
    span: miette::SourceSpan,
    kind: BaseErrorKind<
        &'b str,
        Box<dyn std::error::Error + Send + Sync + 'static>,
    >,
    #[related]
    others: Vec<FormattedErrorContext<'b>>,
}

#[derive(Error, Debug, Diagnostic)]
#[error("Parse Error Context")]
pub struct FormattedErrorContext<'b> {
    #[source_code]
    src: &'b str,
    #[label("{context}")]
    span: miette::SourceSpan,
    context: StackContext<&'b str>,
}

impl<'a> From<(&'a str, ParseError<'a>)> for FormattedError<'a> {
    fn from((input, e): (&'a str, ParseError<'a>)) -> Self {
        match e {
            GenericErrorTree::Base { location, kind } => {
                let offset = location.location_offset().into();
                FormattedError {
                    kind,
                    src: input,
                    span: miette::SourceSpan::new(offset, 0),
                    others: Vec::new(),
                }
            }
            GenericErrorTree::Stack { base, contexts } => {
                let mut base = Self::from((input, *base));
                let mut contexts: Vec<FormattedErrorContext> = contexts
                    .into_iter()
                    .map(|(location, context)| FormattedErrorContext {
                        context,
                        src: input,
                        span: miette::SourceSpan::new(
                            location.location_offset().into(),
                            0,
                        ),
                    })
                    .collect();
                base.others.append(&mut contexts);
                base
            }
            GenericErrorTree::Alt(alt_errors) => alt_errors
                .into_iter()
                .map(|e| Self::from((input, e)))
                .max_by_key(|formatted| formatted.others.len())
                .unwrap(),
        }
    }
}
