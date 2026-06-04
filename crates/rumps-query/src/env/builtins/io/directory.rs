use smallvec::smallvec;

use super::super::super::{Environment, Module, PrimDef};
use crate::primitives::Directory;
use crate::typecheck::{Scheme, Ty, TyArena};

impl Environment {
    /// Build the `Io.Directory` submodule.
    pub(super) fn build_io_directory_module(&mut self) -> Module {
        let a = &mut self.ty_arena;
        let consts = &mut self.consts;

        // Common types for this module
        let fp_to_bool = a.func(smallvec![TyArena::FILEPATH], TyArena::BOOL);
        let fp_to_unit = a.func(smallvec![TyArena::FILEPATH], TyArena::UNIT);
        let list_dir_ret = a.array(TyArena::PATH);
        let list_dir_ty = a.func(smallvec![TyArena::FILEPATH], list_dir_ret);
        let read_file_ty =
            a.func(smallvec![TyArena::FILEPATH], TyArena::STRING);
        let pwd_ty = a.func(smallvec![], TyArena::FILEPATH);
        let get_env_ret = a.option(TyArena::STRING);
        let get_env_ty = a.func(smallvec![TyArena::STRING], get_env_ret);
        let canonicalize_ty =
            a.func(smallvec![TyArena::FILEPATH], TyArena::FILEPATH);
        let parent_ret = a.option(TyArena::FILEPATH);
        let parent_ty = a.func(smallvec![TyArena::FILEPATH], parent_ret);
        let file_name_ret = a.option(TyArena::STRING);
        let file_name_ty = a.func(smallvec![TyArena::FILEPATH], file_name_ret);
        let extension_ret = a.option(TyArena::STRING);
        let extension_ty = a.func(smallvec![TyArena::FILEPATH], extension_ret);
        let arr_str = a.array(TyArena::STRING);
        let join_ty =
            a.func(smallvec![TyArena::FILEPATH, arr_str], TyArena::FILEPATH);
        let temp_dir_ty = a.func(smallvec![], TyArena::FILEPATH);
        let with_ext_ty = a.func(
            smallvec![TyArena::FILEPATH, TyArena::STRING],
            TyArena::FILEPATH,
        );

        // Object types for path-pair operations
        let src_sid = consts.intern("src");
        let dest_sid = consts.intern("dest");
        let path_pair_obj = a.alloc(Ty::Object(indexmap::indexmap! {
            src_sid => TyArena::FILEPATH,
            dest_sid => TyArena::FILEPATH,
        }));
        let move_ty = a.func(smallvec![path_pair_obj], TyArena::UNIT);
        let copy_ty = a.func(smallvec![path_pair_obj], TyArena::UNIT);

        // Object types for file write operations
        let path_sid = consts.intern("path");
        let contents_sid = consts.intern("contents");
        let file_obj = a.alloc(Ty::Object(indexmap::indexmap! {
            path_sid => TyArena::FILEPATH,
            contents_sid => TyArena::STRING,
        }));
        let write_ty = a.func(smallvec![file_obj], TyArena::UNIT);
        let append_ty = a.func(smallvec![file_obj], TyArena::UNIT);

        // Object type for set-env
        let name_sid = consts.intern("name");
        let value_sid = consts.intern("value");
        let env_obj = a.alloc(Ty::Object(indexmap::indexmap! {
            name_sid => TyArena::STRING,
            value_sid => TyArena::STRING,
        }));
        let set_env_ty = a.func(smallvec![env_obj], TyArena::UNIT);

        Module::from_prims(
            &[
                PrimDef {
                    name: "list-dir",
                    f: Directory::list_dir,
                    ty: Scheme::mono(list_dir_ty),
                },
                PrimDef {
                    name: "exists",
                    f: Directory::exists,
                    ty: Scheme::mono(fp_to_bool),
                },
                PrimDef {
                    name: "is-file",
                    f: Directory::is_file,
                    ty: Scheme::mono(fp_to_bool),
                },
                PrimDef {
                    name: "is-dir",
                    f: Directory::is_dir,
                    ty: Scheme::mono(fp_to_bool),
                },
                PrimDef {
                    name: "read-file",
                    f: Directory::read_file,
                    ty: Scheme::mono(read_file_ty),
                },
                PrimDef {
                    name: "remove",
                    f: Directory::remove,
                    ty: Scheme::mono(fp_to_unit),
                },
                PrimDef {
                    name: "remove-all",
                    f: Directory::remove_all,
                    ty: Scheme::mono(fp_to_unit),
                },
                PrimDef {
                    name: "create-dir",
                    f: Directory::create_dir,
                    ty: Scheme::mono(fp_to_unit),
                },
                PrimDef {
                    name: "create-dir-all",
                    f: Directory::create_dir_all,
                    ty: Scheme::mono(fp_to_unit),
                },
                PrimDef {
                    name: "pwd",
                    f: Directory::pwd,
                    ty: Scheme::mono(pwd_ty),
                },
                PrimDef {
                    name: "set-pwd",
                    f: Directory::set_pwd,
                    ty: Scheme::mono(fp_to_unit),
                },
                PrimDef {
                    name: "get-env",
                    f: Directory::get_env,
                    ty: Scheme::mono(get_env_ty),
                },
                PrimDef {
                    name: "move-path",
                    f: Directory::move_path,
                    ty: Scheme::mono(move_ty),
                },
                PrimDef {
                    name: "copy-path",
                    f: Directory::copy_path,
                    ty: Scheme::mono(copy_ty),
                },
                PrimDef {
                    name: "write-file",
                    f: Directory::write_file,
                    ty: Scheme::mono(write_ty),
                },
                PrimDef {
                    name: "append-file",
                    f: Directory::append_file,
                    ty: Scheme::mono(append_ty),
                },
                PrimDef {
                    name: "set-env",
                    f: Directory::set_env,
                    ty: Scheme::mono(set_env_ty),
                },
                PrimDef {
                    name: "canonicalize",
                    f: Directory::canonicalize,
                    ty: Scheme::mono(canonicalize_ty),
                },
                PrimDef {
                    name: "parent",
                    f: Directory::parent,
                    ty: Scheme::mono(parent_ty),
                },
                PrimDef {
                    name: "file-name",
                    f: Directory::file_name,
                    ty: Scheme::mono(file_name_ty),
                },
                PrimDef {
                    name: "extension",
                    f: Directory::extension,
                    ty: Scheme::mono(extension_ty),
                },
                PrimDef {
                    name: "join",
                    f: Directory::join,
                    ty: Scheme::mono(join_ty),
                },
                PrimDef {
                    name: "temp-dir",
                    f: Directory::temp_dir,
                    ty: Scheme::mono(temp_dir_ty),
                },
                PrimDef {
                    name: "with-extension",
                    f: Directory::with_extension,
                    ty: Scheme::mono(with_ext_ty),
                },
            ],
            &mut self.consts.strings,
        )
    }
}
