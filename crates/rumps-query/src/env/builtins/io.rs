mod directory;

use rumps_query_macros::scheme;

use super::super::{Environment, Module};
use crate::builtins::{Def, Impl, Io};

impl Environment {
    pub(super) fn register_io_builtin(&mut self) {
        // Build Directory submodule first to avoid double mutable borrow
        let directory_module = self.build_io_directory_module();

        let a = &mut self.ty_arena;

        let io_id = self.consts.strings.intern("Io");
        let dir_id = self.consts.strings.intern("Directory");
        let io_mod = Module::from_defs(
            &[
                Def {
                    name: "get-line",
                    imp: Impl::Async(Io::get_line),
                    ty: scheme!(a, () -> String),
                },
                Def {
                    name: "print",
                    imp: Impl::Async(Io::print),
                    ty: scheme!(a, (String) -> Unit),
                },
                Def {
                    name: "println",
                    imp: Impl::Async(Io::println),
                    ty: scheme!(a, (String) -> Unit),
                },
                Def {
                    name: "eprint",
                    imp: Impl::Async(Io::eprint),
                    ty: scheme!(a, (String) -> Unit),
                },
                Def {
                    name: "eprintln",
                    imp: Impl::Async(Io::eprintln),
                    ty: scheme!(a, (String) -> Unit),
                },
            ],
            &mut self.consts.strings,
        )
        .with_submodule(dir_id, directory_module);
        self.modules.insert(io_id, io_mod);
    }
}
