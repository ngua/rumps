use super::*;

/// Marker class for numeric types.
pub(crate) struct Numeric;

impl Class for Numeric {
    const ID: ClassId = ClassId::NUMERIC;

    fn register_all(_: &mut ClassMethods, _: &mut StringInterner) {
        // `Numeric` has no runtime methods; it only marks numeric types.
    }
}
