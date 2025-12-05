use smallvec::SmallVec;

use crate::ast::ExprId;
use crate::intern::StringId;
use crate::typecheck::RuntimeTyId;
use crate::value::ValueId;
use crate::{ClassId, Span};

#[derive(Clone)]
pub(crate) struct Dispatch {
    pub(in crate::interpreter) dispatch_expr_id: Option<ExprId>,
    pub(in crate::interpreter) output_expr_id: Option<ExprId>,
    pub(in crate::interpreter) output_ty: Option<RuntimeTyId>,
    pub(in crate::interpreter) class: ClassId,
    pub(in crate::interpreter) method: StringId,
    pub(in crate::interpreter) args: SmallVec<[ValueId; 4]>,
    pub(in crate::interpreter) span: Span,
}

impl Dispatch {
    pub(crate) fn internal(
        class: ClassId,
        method: StringId,
        args: SmallVec<[ValueId; 4]>,
        output_ty: Option<RuntimeTyId>,
        span: Span,
    ) -> Self {
        Self {
            dispatch_expr_id: None,
            output_expr_id: None,
            output_ty,
            class,
            method,
            args,
            span,
        }
    }
}
