use std::env;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use smallvec::{smallvec, SmallVec};

use super::Prim;
use crate::env::{PrimCtx, PrimResult};
use crate::value::{Payload, ValueId};

/// Primitives for the `Io.Directory` submodule.
///
/// Provides file system operations using `tokio::fs` for async I/O.
pub(crate) struct Directory;

impl Prim for Directory {}

impl Directory {
    /// Helper: extract path string from a `Payload::FilePath`.
    fn get_path_str(ctx: &PrimCtx<'_>, id: ValueId) -> String {
        ctx.arena
            .payload(id)
            .and_then(|v| match v {
                Payload::FilePath(sid) => ctx.arena.get_str(*sid),
                _ => None,
            })
            .unwrap_or_else(|| typechecked!("Io.Directory", "FilePath"))
            .to_owned()
    }

    /// Helper: create a `Path.File(filepath)` value.
    fn make_path_file(ctx: &mut PrimCtx<'_>, path_str: &str) -> ValueId {
        let sid = ctx.arena.intern(path_str);
        let fp_id = ctx.arena.add_typed(
            Payload::FilePath(sid),
            ctx.runtime_types.meta_filepath(),
            ctx.span,
        );
        ctx.arena.add_typed(
            Payload::Variant {
                tag: 0,
                vals: smallvec![fp_id],
            },
            ctx.runtime_types.meta_path(),
            ctx.span,
        )
    }

    /// Helper: create a `Path.Dir(filepath)` value.
    fn make_path_dir(ctx: &mut PrimCtx<'_>, path_str: &str) -> ValueId {
        let sid = ctx.arena.intern(path_str);
        let fp_id = ctx.arena.add_typed(
            Payload::FilePath(sid),
            ctx.runtime_types.meta_filepath(),
            ctx.span,
        );
        ctx.arena.add_typed(
            Payload::Variant {
                tag: 1,
                vals: smallvec![fp_id],
            },
            ctx.runtime_types.meta_path(),
            ctx.span,
        )
    }

    /// `(FilePath) -> Array[Path]`
    ///
    /// Lists the contents of a directory.
    pub(crate) fn list_dir<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let path_str = Self::get_path_str(ctx, args[0]);
            let mut entries =
                tokio::fs::read_dir(&path_str).await.map_err(|e| {
                    ctx.runtime_error(format!("Io.Directory.list-dir: {e}"))
                })?;

            let mut paths: SmallVec<[ValueId; 4]> = SmallVec::new();
            loop {
                match entries.next_entry().await {
                    Ok(Some(entry)) => {
                        let p = entry.path();
                        let path_string = p.to_string_lossy().to_string();
                        let path_id = match entry.file_type().await {
                            Ok(ft) if ft.is_dir() => {
                                Self::make_path_dir(ctx, &path_string)
                            }
                            _ => Self::make_path_file(ctx, &path_string),
                        };
                        paths.push(path_id);
                    }
                    Ok(None) => break,
                    Err(e) => Err(ctx
                        .runtime_error(format!("Io.Directory.list-dir: {e}")))?,
                }
            }

            Ok(ctx.add(Payload::Array(Arc::new(paths))))
        })
    }

    /// `({ src: FilePath, dest: FilePath }) -> Unit`
    ///
    /// Moves or renames a file or directory.
    pub(crate) fn move_path<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let obj = ctx.arena.payload(args[0]).cloned().ok_or_else(|| {
                ctx.runtime_error("Io.Directory.move-path: invalid object")
            })?;

            let (src, dest) = match obj {
                Payload::Object(fields) => {
                    let src_id =
                        fields.values().next().copied().ok_or_else(|| {
                            ctx.runtime_error(
                                "Io.Directory.move-path: missing src",
                            )
                        })?;
                    let dest_id =
                        fields.values().nth(1).copied().ok_or_else(|| {
                            ctx.runtime_error(
                                "Io.Directory.move-path: missing dest",
                            )
                        })?;
                    (
                        Self::get_path_str(ctx, src_id),
                        Self::get_path_str(ctx, dest_id).to_string(),
                    )
                }
                _ => typechecked!(
                    "Io.Directory.move-path",
                    "{ src: FilePath, dest: FilePath }"
                ),
            };

            tokio::fs::rename(&src, &dest).await.map_err(|e| {
                ctx.runtime_error(format!("Io.Directory.move-path: {e}"))
            })?;

            Ok(ctx.arena.add_typed(
                Payload::Unit,
                ctx.runtime_types.meta_unit(),
                ctx.span,
            ))
        })
    }

    /// `({ src: FilePath, dest: FilePath }) -> Unit`
    ///
    /// Copies a file. For directories, use recursive copy (not yet implemented).
    pub(crate) fn copy_path<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let obj = ctx.arena.payload(args[0]).cloned().ok_or_else(|| {
                ctx.runtime_error("Io.Directory.copy-path: invalid object")
            })?;

            let (src, dest) = match obj {
                Payload::Object(fields) => {
                    let src_id =
                        fields.values().next().copied().ok_or_else(|| {
                            ctx.runtime_error(
                                "Io.Directory.copy-path: missing src",
                            )
                        })?;
                    let dest_id =
                        fields.values().nth(1).copied().ok_or_else(|| {
                            ctx.runtime_error(
                                "Io.Directory.copy-path: missing dest",
                            )
                        })?;
                    (
                        Self::get_path_str(ctx, src_id),
                        Self::get_path_str(ctx, dest_id).to_string(),
                    )
                }
                _ => typechecked!(
                    "Io.Directory.copy-path",
                    "{ src: FilePath, dest: FilePath }"
                ),
            };

            tokio::fs::copy(&src, &dest).await.map_err(|e| {
                ctx.runtime_error(format!("Io.Directory.copy-path: {e}"))
            })?;

            Ok(ctx.arena.add_typed(
                Payload::Unit,
                ctx.runtime_types.meta_unit(),
                ctx.span,
            ))
        })
    }

    /// `(FilePath) -> Unit`
    ///
    /// Removes a file or empty directory.
    pub(crate) fn remove<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let path_str = Self::get_path_str(ctx, args[0]);

            // Try removing as file first, then as directory
            let result = tokio::fs::remove_file(&path_str).await;
            match result {
                Ok(()) => Ok(ctx.arena.add_typed(
                    Payload::Unit,
                    ctx.runtime_types.meta_unit(),
                    ctx.span,
                )),
                Err(_) => {
                    tokio::fs::remove_dir(&path_str).await.map_err(|e| {
                        ctx.runtime_error(format!("Io.Directory.remove: {e}"))
                    })?;
                    Ok(ctx.arena.add_typed(
                        Payload::Unit,
                        ctx.runtime_types.meta_unit(),
                        ctx.span,
                    ))
                }
            }
        })
    }

    /// `(FilePath) -> Unit`
    ///
    /// Recursively removes a file or directory.
    pub(crate) fn remove_all<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let path_str = Self::get_path_str(ctx, args[0]);

            // Try removing as file first
            let result = tokio::fs::remove_file(&path_str).await;
            match result {
                Ok(()) => Ok(ctx.arena.add_typed(
                    Payload::Unit,
                    ctx.runtime_types.meta_unit(),
                    ctx.span,
                )),
                Err(_) => {
                    tokio::fs::remove_dir_all(&path_str).await.map_err(
                        |e| {
                            ctx.runtime_error(format!(
                                "Io.Directory.remove-all: {e}"
                            ))
                        },
                    )?;
                    Ok(ctx.arena.add_typed(
                        Payload::Unit,
                        ctx.runtime_types.meta_unit(),
                        ctx.span,
                    ))
                }
            }
        })
    }

    /// `(FilePath) -> Bool`
    ///
    /// Checks if a path exists.
    pub(crate) fn exists<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let path_str = Self::get_path_str(ctx, args[0]);
            let exists =
                tokio::fs::try_exists(&path_str).await.unwrap_or(false);
            Ok(ctx.arena.add_typed(
                Payload::Bool(exists),
                ctx.runtime_types.meta_bool(),
                ctx.span,
            ))
        })
    }

    /// `(FilePath) -> Bool`
    ///
    /// Checks if a path is a file.
    pub(crate) fn is_file<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let path_str = Self::get_path_str(ctx, args[0]);
            let is_file = tokio::fs::metadata(&path_str)
                .await
                .map(|m| m.is_file())
                .unwrap_or(false);
            Ok(ctx.arena.add_typed(
                Payload::Bool(is_file),
                ctx.runtime_types.meta_bool(),
                ctx.span,
            ))
        })
    }

    /// `(FilePath) -> Bool`
    ///
    /// Checks if a path is a directory.
    pub(crate) fn is_dir<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let path_str = Self::get_path_str(ctx, args[0]);
            let is_dir = tokio::fs::metadata(&path_str)
                .await
                .map(|m| m.is_dir())
                .unwrap_or(false);
            Ok(ctx.arena.add_typed(
                Payload::Bool(is_dir),
                ctx.runtime_types.meta_bool(),
                ctx.span,
            ))
        })
    }

    /// `(FilePath) -> String`
    ///
    /// Reads the entire contents of a file as a string.
    pub(crate) fn read_file<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let path_str = Self::get_path_str(ctx, args[0]);
            let contents =
                tokio::fs::read_to_string(&path_str).await.map_err(|e| {
                    ctx.runtime_error(format!("Io.Directory.read-file: {e}"))
                })?;
            let sid = ctx.arena.intern(&contents);
            Ok(ctx.arena.add_typed(
                Payload::String(sid),
                ctx.runtime_types.meta_string(),
                ctx.span,
            ))
        })
    }

    /// `({ path: FilePath, contents: String }) -> Unit`
    ///
    /// Writes a string to a file, creating or overwriting it.
    pub(crate) fn write_file<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let obj = ctx.arena.payload(args[0]).cloned().ok_or_else(|| {
                ctx.runtime_error("Io.Directory.write-file: invalid object")
            })?;

            let (path, contents) = match obj {
                Payload::Object(fields) => {
                    let path_id =
                        fields.values().next().copied().ok_or_else(|| {
                            ctx.runtime_error(
                                "Io.Directory.write-file: missing path",
                            )
                        })?;
                    let contents_id =
                        fields.values().nth(1).copied().ok_or_else(|| {
                            ctx.runtime_error(
                                "Io.Directory.write-file: missing contents",
                            )
                        })?;
                    let path_str = Self::get_path_str(ctx, path_id).to_string();
                    let contents_str = ctx
                        .arena
                        .get_string_id(contents_id)
                        .and_then(|sid| ctx.arena.get_str(sid))
                        .unwrap_or_else(|| {
                            typechecked!("Io.Directory.write-file", "String")
                        })
                        .to_string();
                    (path_str, contents_str)
                }
                _ => typechecked!(
                    "Io.Directory.write-file",
                    "{ path: FilePath, contents: String }"
                ),
            };

            tokio::fs::write(&path, &contents).await.map_err(|e| {
                ctx.runtime_error(format!("Io.Directory.write-file: {e}"))
            })?;

            Ok(ctx.arena.add_typed(
                Payload::Unit,
                ctx.runtime_types.meta_unit(),
                ctx.span,
            ))
        })
    }

    /// `({ path: FilePath, contents: String }) -> Unit`
    ///
    /// Appends a string to a file.
    pub(crate) fn append_file<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            use tokio::io::AsyncWriteExt;

            let obj = ctx.arena.payload(args[0]).cloned().ok_or_else(|| {
                ctx.runtime_error("Io.Directory.append-file: invalid object")
            })?;

            let (path, contents) = match obj {
                Payload::Object(fields) => {
                    let path_id =
                        fields.values().next().copied().ok_or_else(|| {
                            ctx.runtime_error(
                                "Io.Directory.append-file: missing path",
                            )
                        })?;
                    let contents_id =
                        fields.values().nth(1).copied().ok_or_else(|| {
                            ctx.runtime_error(
                                "Io.Directory.append-file: missing contents",
                            )
                        })?;
                    let path_str = Self::get_path_str(ctx, path_id).to_string();
                    let contents_str = ctx
                        .arena
                        .get_string_id(contents_id)
                        .and_then(|sid| ctx.arena.get_str(sid))
                        .unwrap_or_else(|| {
                            typechecked!("Io.Directory.append-file", "String")
                        })
                        .to_string();
                    (path_str, contents_str)
                }
                _ => typechecked!(
                    "Io.Directory.append-file",
                    "{ path: FilePath, contents: String }"
                ),
            };

            let mut file = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .await
                .map_err(|e| {
                    ctx.runtime_error(format!("Io.Directory.append-file: {e}"))
                })?;

            file.write_all(contents.as_bytes()).await.map_err(|e| {
                ctx.runtime_error(format!("Io.Directory.append-file: {e}"))
            })?;

            Ok(ctx.arena.add_typed(
                Payload::Unit,
                ctx.runtime_types.meta_unit(),
                ctx.span,
            ))
        })
    }

    /// `(FilePath) -> Unit`
    ///
    /// Creates a directory.
    pub(crate) fn create_dir<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let path_str = Self::get_path_str(ctx, args[0]);
            tokio::fs::create_dir(&path_str).await.map_err(|e| {
                ctx.runtime_error(format!("Io.Directory.create-dir: {e}"))
            })?;
            Ok(ctx.arena.add_typed(
                Payload::Unit,
                ctx.runtime_types.meta_unit(),
                ctx.span,
            ))
        })
    }

    /// `(FilePath) -> Unit`
    ///
    /// Creates a directory and all parent directories.
    pub(crate) fn create_dir_all<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let path_str = Self::get_path_str(ctx, args[0]);
            tokio::fs::create_dir_all(&path_str).await.map_err(|e| {
                ctx.runtime_error(format!("Io.Directory.create-dir-all: {e}"))
            })?;
            Ok(ctx.arena.add_typed(
                Payload::Unit,
                ctx.runtime_types.meta_unit(),
                ctx.span,
            ))
        })
    }

    /// `() -> FilePath`
    ///
    /// Returns the current working directory.
    pub(crate) fn pwd<'a>(
        ctx: &'a mut PrimCtx<'a>,
        _args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let cwd = env::current_dir().map_err(|e| {
                ctx.runtime_error(format!("Io.Directory.pwd: {e}"))
            })?;
            let path_str = cwd.to_string_lossy();
            let sid = ctx.arena.intern(&path_str);
            Ok(ctx.arena.add_typed(
                Payload::FilePath(sid),
                ctx.runtime_types.meta_filepath(),
                ctx.span,
            ))
        })
    }

    /// `(FilePath) -> Unit`
    ///
    /// Changes the current working directory.
    pub(crate) fn set_pwd<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let path_str = Self::get_path_str(ctx, args[0]);
            env::set_current_dir(&path_str).map_err(|e| {
                ctx.runtime_error(format!("Io.Directory.set-pwd: {e}"))
            })?;
            Ok(ctx.arena.add_typed(
                Payload::Unit,
                ctx.runtime_types.meta_unit(),
                ctx.span,
            ))
        })
    }

    /// `(String) -> Option[String]`
    ///
    /// Gets an environment variable.
    pub(crate) fn get_env<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let name_sid =
                ctx.arena.get_string_id(args[0]).unwrap_or_else(|| {
                    typechecked!("Io.Directory.get-env", "String")
                });
            let name = Self::valid_str(ctx.arena, name_sid);

            match env::var(name) {
                Ok(val) => {
                    let sid = ctx.arena.intern(&val);
                    let val_id = ctx.arena.add_typed(
                        Payload::String(sid),
                        ctx.runtime_types.meta_string(),
                        ctx.span,
                    );
                    Ok(ctx.option_some(val_id))
                }
                Err(_) => Ok(ctx.option_none()),
            }
        })
    }

    /// `({ name: String, value: String }) -> Unit`
    ///
    /// Sets an environment variable.
    pub(crate) fn set_env<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let obj = ctx.arena.payload(args[0]).cloned().ok_or_else(|| {
                ctx.runtime_error("Io.Directory.set-env: invalid object")
            })?;

            let (name, value) = match obj {
                Payload::Object(fields) => {
                    let name_id =
                        fields.values().next().copied().ok_or_else(|| {
                            ctx.runtime_error(
                                "Io.Directory.set-env: missing name",
                            )
                        })?;
                    let value_id =
                        fields.values().nth(1).copied().ok_or_else(|| {
                            ctx.runtime_error(
                                "Io.Directory.set-env: missing value",
                            )
                        })?;
                    let name_str = ctx
                        .arena
                        .get_string_id(name_id)
                        .and_then(|sid| ctx.arena.get_str(sid))
                        .unwrap_or_else(|| {
                            typechecked!("Io.Directory.set-env", "String")
                        })
                        .to_string();
                    let value_str = ctx
                        .arena
                        .get_string_id(value_id)
                        .and_then(|sid| ctx.arena.get_str(sid))
                        .unwrap_or_else(|| {
                            typechecked!("Io.Directory.set-env", "String")
                        })
                        .to_string();
                    (name_str, value_str)
                }
                _ => typechecked!(
                    "Io.Directory.set-env",
                    "{ name: String, value: String }"
                ),
            };

            env::set_var(&name, &value);
            Ok(ctx.arena.add_typed(
                Payload::Unit,
                ctx.runtime_types.meta_unit(),
                ctx.span,
            ))
        })
    }

    /// `(FilePath) -> FilePath`
    ///
    /// Resolves a path to its absolute, canonical form.
    pub(crate) fn canonicalize<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let path_str = Self::get_path_str(ctx, args[0]);
            let canonical =
                tokio::fs::canonicalize(&path_str).await.map_err(|e| {
                    ctx.runtime_error(format!("Io.Directory.canonicalize: {e}"))
                })?;
            let sid = ctx.arena.intern(&canonical.to_string_lossy());
            Ok(ctx.arena.add_typed(
                Payload::FilePath(sid),
                ctx.runtime_types.meta_filepath(),
                ctx.span,
            ))
        })
    }

    /// `(FilePath) -> Option[FilePath]`
    ///
    /// Returns the parent directory of a path.
    pub(crate) fn parent<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let path_str = Self::get_path_str(ctx, args[0]);
            let path = Path::new(&path_str);

            match path.parent() {
                Some(p) if !p.as_os_str().is_empty() => {
                    let sid = ctx.arena.intern(&p.to_string_lossy());
                    let fp = ctx.arena.add_typed(
                        Payload::FilePath(sid),
                        ctx.runtime_types.meta_filepath(),
                        ctx.span,
                    );
                    Ok(ctx.option_some(fp))
                }
                _ => Ok(ctx.option_none()),
            }
        })
    }

    /// `(FilePath) -> Option[String]`
    ///
    /// Returns the final component of a path.
    pub(crate) fn file_name<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let path_str = Self::get_path_str(ctx, args[0]);
            let path = Path::new(&path_str);
            let name_opt =
                path.file_name().map(|n| n.to_string_lossy().to_string());

            match name_opt {
                Some(name) => {
                    let sid = ctx.arena.intern(&name);
                    let s = ctx.arena.add_typed(
                        Payload::String(sid),
                        ctx.runtime_types.meta_string(),
                        ctx.span,
                    );
                    Ok(ctx.option_some(s))
                }
                None => Ok(ctx.option_none()),
            }
        })
    }

    /// `(FilePath) -> Option[String]`
    ///
    /// Returns the file extension, if any.
    pub(crate) fn extension<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let path_str = Self::get_path_str(ctx, args[0]);
            let path = Path::new(&path_str);
            let ext_opt =
                path.extension().map(|e| e.to_string_lossy().to_string());

            match ext_opt {
                Some(ext) => {
                    let sid = ctx.arena.intern(&ext);
                    let s = ctx.arena.add_typed(
                        Payload::String(sid),
                        ctx.runtime_types.meta_string(),
                        ctx.span,
                    );
                    Ok(ctx.option_some(s))
                }
                None => Ok(ctx.option_none()),
            }
        })
    }

    /// `(FilePath, Array[String]) -> FilePath`
    ///
    /// Joins path components.
    pub(crate) fn join<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let base_str = Self::get_path_str(ctx, args[0]);

            let parts =
                ctx.arena.payload(args[1]).cloned().ok_or_else(|| {
                    ctx.runtime_error("Io.Directory.join: invalid array")
                })?;

            // Collect all parts as owned strings first
            let part_strs: Vec<String> = match parts {
                Payload::Array(elems) => elems
                    .iter()
                    .map(|elem_id| {
                        let sid = ctx
                            .arena
                            .get_string_id(*elem_id)
                            .unwrap_or_else(|| {
                                typechecked!(
                                    "Io.Directory.join",
                                    "String element"
                                )
                            });
                        Self::valid_str(ctx.arena, sid).to_owned()
                    })
                    .collect(),
                _ => typechecked!("Io.Directory.join", "Array[String]"),
            };

            let mut path = PathBuf::from(&base_str);
            part_strs.iter().for_each(|p| path.push(p));

            let sid = ctx.arena.intern(&path.to_string_lossy());
            Ok(ctx.arena.add_typed(
                Payload::FilePath(sid),
                ctx.runtime_types.meta_filepath(),
                ctx.span,
            ))
        })
    }

    /// `() -> FilePath`
    ///
    /// Returns the system temporary directory.
    pub(crate) fn temp_dir<'a>(
        ctx: &'a mut PrimCtx<'a>,
        _args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let tmp = env::temp_dir();
            let sid = ctx.arena.intern(&tmp.to_string_lossy());
            Ok(ctx.arena.add_typed(
                Payload::FilePath(sid),
                ctx.runtime_types.meta_filepath(),
                ctx.span,
            ))
        })
    }

    /// `(FilePath, String) -> FilePath`
    ///
    /// Returns a new path with the given extension.
    pub(crate) fn with_extension<'a>(
        ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move {
            let path_str = Self::get_path_str(ctx, args[0]);
            let ext = ctx
                .arena
                .get_string_id(args[1])
                .and_then(|sid| ctx.arena.get_str(sid))
                .map(String::from)
                .ok_or_else(|| {
                    ctx.runtime_error(
                        "Io.Directory.with-extension: invalid extension",
                    )
                })?;

            let mut path = PathBuf::from(&path_str);
            path.set_extension(&ext);

            let sid = ctx.arena.intern(&path.to_string_lossy());
            Ok(ctx.arena.add_typed(
                Payload::FilePath(sid),
                ctx.runtime_types.meta_filepath(),
                ctx.span,
            ))
        })
    }
}
