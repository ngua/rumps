use rumps_query_macros::scheme;

use super::super::super::{Environment, Module};
use crate::builtins::{Def, Directory, Impl};

impl Environment {
    /// Build the `Io.Directory` submodule.
    pub(super) fn build_io_directory_module(&mut self) -> Module {
        let a = &mut self.ty_arena;

        let (mv_sc, cp_sc, wr_sc, app_sc, set_env_sc) = {
            let mut intern = |s| self.consts.strings.intern(s);
            (
                scheme!(
                    a,
                    intern,
                    ({ src: FilePath, dest: FilePath }) -> Unit
                ),
                scheme!(
                    a,
                    intern,
                    ({ src: FilePath, dest: FilePath }) -> Unit
                ),
                scheme!(
                    a,
                    intern,
                    ({ path: FilePath, contents: String }) -> Unit
                ),
                scheme!(
                    a,
                    intern,
                    ({ path: FilePath, contents: String }) -> Unit
                ),
                scheme!(a, intern, ({ name: String, value: String }) -> Unit),
            )
        };

        Module::from_defs(
            &[
                Def {
                    name: "list-dir",
                    imp: Impl::Async(Directory::list_dir),
                    ty: scheme!(a, (FilePath) -> Array[Path]),
                },
                Def {
                    name: "exists",
                    imp: Impl::Async(Directory::exists),
                    ty: scheme!(a, (FilePath) -> Bool),
                },
                Def {
                    name: "is-file",
                    imp: Impl::Async(Directory::is_file),
                    ty: scheme!(a, (FilePath) -> Bool),
                },
                Def {
                    name: "is-dir",
                    imp: Impl::Async(Directory::is_dir),
                    ty: scheme!(a, (FilePath) -> Bool),
                },
                Def {
                    name: "read-file",
                    imp: Impl::Async(Directory::read_file),
                    ty: scheme!(a, (FilePath) -> String),
                },
                Def {
                    name: "remove",
                    imp: Impl::Async(Directory::remove),
                    ty: scheme!(a, (FilePath) -> Unit),
                },
                Def {
                    name: "remove-all",
                    imp: Impl::Async(Directory::remove_all),
                    ty: scheme!(a, (FilePath) -> Unit),
                },
                Def {
                    name: "create-dir",
                    imp: Impl::Async(Directory::create_dir),
                    ty: scheme!(a, (FilePath) -> Unit),
                },
                Def {
                    name: "create-dir-all",
                    imp: Impl::Async(Directory::create_dir_all),
                    ty: scheme!(a, (FilePath) -> Unit),
                },
                Def {
                    name: "pwd",
                    imp: Impl::Sync(Directory::pwd),
                    ty: scheme!(a, () -> FilePath),
                },
                Def {
                    name: "set-pwd",
                    imp: Impl::Sync(Directory::set_pwd),
                    ty: scheme!(a, (FilePath) -> Unit),
                },
                Def {
                    name: "get-env",
                    imp: Impl::Sync(Directory::get_env),
                    ty: scheme!(a, (String) -> Option[String]),
                },
                Def {
                    name: "move-path",
                    imp: Impl::Async(Directory::move_path),
                    ty: mv_sc,
                },
                Def {
                    name: "copy-path",
                    imp: Impl::Async(Directory::copy_path),
                    ty: cp_sc,
                },
                Def {
                    name: "write-file",
                    imp: Impl::Async(Directory::write_file),
                    ty: wr_sc,
                },
                Def {
                    name: "append-file",
                    imp: Impl::Async(Directory::append_file),
                    ty: app_sc,
                },
                Def {
                    name: "set-env",
                    imp: Impl::Sync(Directory::set_env),
                    ty: set_env_sc,
                },
                Def {
                    name: "canonicalize",
                    imp: Impl::Async(Directory::canonicalize),
                    ty: scheme!(a, (FilePath) -> FilePath),
                },
                Def {
                    name: "parent",
                    imp: Impl::Sync(Directory::parent),
                    ty: scheme!(a, (FilePath) -> Option[FilePath]),
                },
                Def {
                    name: "file-name",
                    imp: Impl::Sync(Directory::file_name),
                    ty: scheme!(a, (FilePath) -> Option[String]),
                },
                Def {
                    name: "extension",
                    imp: Impl::Sync(Directory::extension),
                    ty: scheme!(a, (FilePath) -> Option[String]),
                },
                Def {
                    name: "join",
                    imp: Impl::Sync(Directory::join),
                    ty: scheme!(a, (FilePath, Array[String]) -> FilePath),
                },
                Def {
                    name: "temp-dir",
                    imp: Impl::Sync(Directory::temp_dir),
                    ty: scheme!(a, () -> FilePath),
                },
                Def {
                    name: "with-extension",
                    imp: Impl::Sync(Directory::with_extension),
                    ty: scheme!(a, (FilePath, String) -> FilePath),
                },
            ],
            &mut self.consts.strings,
        )
    }
}
