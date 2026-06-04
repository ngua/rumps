mod directory;

use rumps_query_macros::scheme;

use super::super::{Environment, Module, PrimDef};
use crate::primitives::Io;

impl Environment {
    pub(super) fn register_io_builtin(&mut self) {
        // Build Directory submodule first to avoid double mutable borrow
        let directory_module = self.build_io_directory_module();

        let a = &mut self.ty_arena;

        let io_id = self.consts.strings.intern("Io");
        let dir_id = self.consts.strings.intern("Directory");
        let io_mod = Module::from_prims(
            &[
                PrimDef {
                    name: "get-line",
                    f: Io::get_line,
                    ty: scheme!(a, () -> String),
                },
                PrimDef {
                    name: "print",
                    f: Io::print,
                    ty: scheme!(a, (String) -> Unit),
                },
                PrimDef {
                    name: "println",
                    f: Io::println,
                    ty: scheme!(a, (String) -> Unit),
                },
                PrimDef {
                    name: "eprint",
                    f: Io::eprint,
                    ty: scheme!(a, (String) -> Unit),
                },
                PrimDef {
                    name: "eprintln",
                    f: Io::eprintln,
                    ty: scheme!(a, (String) -> Unit),
                },
            ],
            &mut self.consts.strings,
        )
        .with_submodule(dir_id, directory_module);
        self.modules.insert(io_id, io_mod);
    }
}
