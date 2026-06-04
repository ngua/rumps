use rumps_query_macros::scheme;

use super::super::super::{Environment, Module, PrimDef};
use crate::primitives::Directory;

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

        Module::from_prims(
            &[
                PrimDef {
                    name: "list-dir",
                    f: Directory::list_dir,
                    ty: scheme!(a, (FilePath) -> Array[Path]),
                },
                PrimDef {
                    name: "exists",
                    f: Directory::exists,
                    ty: scheme!(a, (FilePath) -> Bool),
                },
                PrimDef {
                    name: "is-file",
                    f: Directory::is_file,
                    ty: scheme!(a, (FilePath) -> Bool),
                },
                PrimDef {
                    name: "is-dir",
                    f: Directory::is_dir,
                    ty: scheme!(a, (FilePath) -> Bool),
                },
                PrimDef {
                    name: "read-file",
                    f: Directory::read_file,
                    ty: scheme!(a, (FilePath) -> String),
                },
                PrimDef {
                    name: "remove",
                    f: Directory::remove,
                    ty: scheme!(a, (FilePath) -> Unit),
                },
                PrimDef {
                    name: "remove-all",
                    f: Directory::remove_all,
                    ty: scheme!(a, (FilePath) -> Unit),
                },
                PrimDef {
                    name: "create-dir",
                    f: Directory::create_dir,
                    ty: scheme!(a, (FilePath) -> Unit),
                },
                PrimDef {
                    name: "create-dir-all",
                    f: Directory::create_dir_all,
                    ty: scheme!(a, (FilePath) -> Unit),
                },
                PrimDef {
                    name: "pwd",
                    f: Directory::pwd,
                    ty: scheme!(a, () -> FilePath),
                },
                PrimDef {
                    name: "set-pwd",
                    f: Directory::set_pwd,
                    ty: scheme!(a, (FilePath) -> Unit),
                },
                PrimDef {
                    name: "get-env",
                    f: Directory::get_env,
                    ty: scheme!(a, (String) -> Option[String]),
                },
                PrimDef {
                    name: "move-path",
                    f: Directory::move_path,
                    ty: mv_sc,
                },
                PrimDef {
                    name: "copy-path",
                    f: Directory::copy_path,
                    ty: cp_sc,
                },
                PrimDef {
                    name: "write-file",
                    f: Directory::write_file,
                    ty: wr_sc,
                },
                PrimDef {
                    name: "append-file",
                    f: Directory::append_file,
                    ty: app_sc,
                },
                PrimDef {
                    name: "set-env",
                    f: Directory::set_env,
                    ty: set_env_sc,
                },
                PrimDef {
                    name: "canonicalize",
                    f: Directory::canonicalize,
                    ty: scheme!(a, (FilePath) -> FilePath),
                },
                PrimDef {
                    name: "parent",
                    f: Directory::parent,
                    ty: scheme!(a, (FilePath) -> Option[FilePath]),
                },
                PrimDef {
                    name: "file-name",
                    f: Directory::file_name,
                    ty: scheme!(a, (FilePath) -> Option[String]),
                },
                PrimDef {
                    name: "extension",
                    f: Directory::extension,
                    ty: scheme!(a, (FilePath) -> Option[String]),
                },
                PrimDef {
                    name: "join",
                    f: Directory::join,
                    ty: scheme!(a, (FilePath, Array[String]) -> FilePath),
                },
                PrimDef {
                    name: "temp-dir",
                    f: Directory::temp_dir,
                    ty: scheme!(a, () -> FilePath),
                },
                PrimDef {
                    name: "with-extension",
                    f: Directory::with_extension,
                    ty: scheme!(a, (FilePath, String) -> FilePath),
                },
            ],
            &mut self.consts.strings,
        )
    }
}
