use std::env;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::future::BoxFuture;
use indexmap::IndexMap;
use smallvec::{smallvec, SmallVec};
use tokio::io::AsyncWriteExt;

use super::Body;
use crate::builtins::BuiltinCtx;
use crate::intern::StringId;
use crate::value::{Payload, TypeId, ValueId};
use crate::Result;

/// Builtins for the `Io.Directory` submodule.
///
/// Provides file system operations using `tokio::fs` for async I/O.
pub(crate) struct Directory;

impl Body for Directory {}

impl Directory {
    /// `(FilePath) -> Array[Path]`
    ///
    /// Lists the contents of a directory.
    pub(crate) fn list_dir<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let a = args[0];
            let path_str = Self::path_str(ctx, a, "Io.Directory.list-dir")?;
            let mut entries =
                tokio::fs::read_dir(&path_str).await.map_err(|e| {
                    ctx.runtime_error(format!("Io.Directory.list-dir: {e}"))
                })?;
            let mut paths: SmallVec<[ValueId; 4]> = SmallVec::new();

            while let Some(entry) = entries.next_entry().await.map_err(|e| {
                ctx.runtime_error(format!("Io.Directory.list-dir: {e}"))
            })? {
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

            Ok(ctx.vals().add(Payload::Array(Arc::new(paths))))
        })
    }

    /// `({ src: FilePath, dest: FilePath }) -> Unit`
    ///
    /// Moves or renames a file or directory.
    pub(crate) fn move_path<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let a = args[0];
            let (src, dest) =
                Self::path_pair(ctx, a, "Io.Directory.move-path")?;

            tokio::fs::rename(&src, &dest).await.map_err(|e| {
                ctx.runtime_error(format!("Io.Directory.move-path: {e}"))
            })?;

            Ok(ctx.vals().add(Payload::Unit))
        })
    }

    /// `({ src: FilePath, dest: FilePath }) -> Unit`
    ///
    /// Copies a file. For directories, use recursive copy, not yet implemented.
    pub(crate) fn copy_path<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let a = args[0];
            let (src, dest) =
                Self::path_pair(ctx, a, "Io.Directory.copy-path")?;

            tokio::fs::copy(&src, &dest).await.map_err(|e| {
                ctx.runtime_error(format!("Io.Directory.copy-path: {e}"))
            })?;

            Ok(ctx.vals().add(Payload::Unit))
        })
    }

    /// `(FilePath) -> Unit`
    ///
    /// Removes a file or empty directory.
    pub(crate) fn remove<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let a = args[0];
            let path_str = Self::path_str(ctx, a, "Io.Directory.remove")?;
            let result = tokio::fs::remove_file(&path_str).await;

            match result {
                Ok(()) => Ok(ctx.vals().add(Payload::Unit)),
                Err(_) => {
                    tokio::fs::remove_dir(&path_str).await.map_err(|e| {
                        ctx.runtime_error(format!("Io.Directory.remove: {e}"))
                    })?;
                    Ok(ctx.vals().add(Payload::Unit))
                }
            }
        })
    }

    /// `(FilePath) -> Unit`
    ///
    /// Recursively removes a file or directory.
    pub(crate) fn remove_all<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let a = args[0];
            let path_str = Self::path_str(ctx, a, "Io.Directory.remove-all")?;
            let result = tokio::fs::remove_file(&path_str).await;

            match result {
                Ok(()) => Ok(ctx.vals().add(Payload::Unit)),
                Err(_) => {
                    tokio::fs::remove_dir_all(&path_str).await.map_err(
                        |e| {
                            ctx.runtime_error(format!(
                                "Io.Directory.remove-all: {e}"
                            ))
                        },
                    )?;
                    Ok(ctx.vals().add(Payload::Unit))
                }
            }
        })
    }

    /// `(FilePath) -> Bool`
    ///
    /// Checks if a path exists.
    pub(crate) fn exists<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let a = args[0];
            let path_str = Self::path_str(ctx, a, "Io.Directory.exists")?;
            let exists =
                tokio::fs::try_exists(&path_str).await.map_err(|e| {
                    ctx.runtime_error(format!("Io.Directory.exists: {e}"))
                })?;

            Ok(ctx.vals().add(Payload::Bool(exists)))
        })
    }

    /// `(FilePath) -> Bool`
    ///
    /// Checks if a path is a file.
    pub(crate) fn is_file<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let a = args[0];
            let path_str = Self::path_str(ctx, a, "Io.Directory.is-file")?;
            let is_file = tokio::fs::metadata(&path_str)
                .await
                .map(|m| m.is_file())
                .unwrap_or(false);

            Ok(ctx.vals().add(Payload::Bool(is_file)))
        })
    }

    /// `(FilePath) -> Bool`
    ///
    /// Checks if a path is a directory.
    pub(crate) fn is_dir<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let a = args[0];
            let path_str = Self::path_str(ctx, a, "Io.Directory.is-dir")?;
            let is_dir = tokio::fs::metadata(&path_str)
                .await
                .map(|m| m.is_dir())
                .unwrap_or(false);

            Ok(ctx.vals().add(Payload::Bool(is_dir)))
        })
    }

    /// `(FilePath) -> String`
    ///
    /// Reads the entire contents of a file as a string.
    pub(crate) fn read_file<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let a = args[0];
            let path_str = Self::path_str(ctx, a, "Io.Directory.read-file")?;
            let contents =
                tokio::fs::read_to_string(&path_str).await.map_err(|e| {
                    ctx.runtime_error(format!("Io.Directory.read-file: {e}"))
                })?;
            let sid = ctx.vals().intern(&contents);

            Ok(ctx.vals().add(Payload::String(sid)))
        })
    }

    /// `({ path: FilePath, contents: String }) -> Unit`
    ///
    /// Writes a string to a file, creating or overwriting it.
    pub(crate) fn write_file<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let a = args[0];
            let (path, contents) =
                Self::path_contents(ctx, a, "Io.Directory.write-file")?;

            tokio::fs::write(&path, &contents).await.map_err(|e| {
                ctx.runtime_error(format!("Io.Directory.write-file: {e}"))
            })?;

            Ok(ctx.vals().add(Payload::Unit))
        })
    }

    /// `({ path: FilePath, contents: String }) -> Unit`
    ///
    /// Appends a string to a file.
    pub(crate) fn append_file<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let a = args[0];
            let (path, contents) =
                Self::path_contents(ctx, a, "Io.Directory.append-file")?;
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

            Ok(ctx.vals().add(Payload::Unit))
        })
    }

    /// `(FilePath) -> Unit`
    ///
    /// Creates a directory.
    pub(crate) fn create_dir<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let a = args[0];
            let path_str = Self::path_str(ctx, a, "Io.Directory.create-dir")?;

            tokio::fs::create_dir(&path_str).await.map_err(|e| {
                ctx.runtime_error(format!("Io.Directory.create-dir: {e}"))
            })?;
            Ok(ctx.vals().add(Payload::Unit))
        })
    }

    /// `(FilePath) -> Unit`
    ///
    /// Creates a directory and all parent directories.
    pub(crate) fn create_dir_all<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let a = args[0];
            let path_str =
                Self::path_str(ctx, a, "Io.Directory.create-dir-all")?;

            tokio::fs::create_dir_all(&path_str).await.map_err(|e| {
                ctx.runtime_error(format!("Io.Directory.create-dir-all: {e}"))
            })?;
            Ok(ctx.vals().add(Payload::Unit))
        })
    }

    /// `() -> FilePath`
    ///
    /// Returns the current working directory.
    pub(crate) fn pwd(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        _args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let cwd = env::current_dir()
            .map_err(|e| ctx.runtime_error(format!("Io.Directory.pwd: {e}")))?;
        let path_str = cwd.to_string_lossy();
        let sid = ctx.vals().intern(&path_str);

        Ok(ctx.vals().add(Payload::FilePath(sid)))
    }

    /// `(FilePath) -> Unit`
    ///
    /// Changes the current working directory.
    pub(crate) fn set_pwd(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let path_str = Self::path_str(ctx, a, "Io.Directory.set-pwd")?;

        env::set_current_dir(&path_str).map_err(|e| {
            ctx.runtime_error(format!("Io.Directory.set-pwd: {e}"))
        })?;
        Ok(ctx.vals().add(Payload::Unit))
    }

    /// `(String) -> Option[String]`
    ///
    /// Gets an environment variable.
    pub(crate) fn get_env(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let name = Self::string(ctx, args[0], "Io.Directory.get-env")?;

        match env::var(name) {
            Ok(val) => {
                let sid = ctx.vals().intern(&val);
                let val_id = ctx.vals().add(Payload::String(sid));
                Ok(ctx.vals().option_some(val_id))
            }
            Err(_) => Ok(ctx.vals().option_none()),
        }
    }

    /// `({ name: String, value: String }) -> Unit`
    ///
    /// Sets an environment variable.
    pub(crate) fn set_env(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let (name, value) = Self::string_pair(ctx, a, "Io.Directory.set-env")?;

        env::set_var(&name, &value);
        Ok(ctx.vals().add(Payload::Unit))
    }

    /// `(FilePath) -> FilePath`
    ///
    /// Resolves a path to its absolute, canonical form.
    pub(crate) fn canonicalize<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let a = args[0];
            let path_str = Self::path_str(ctx, a, "Io.Directory.canonicalize")?;
            let canonical =
                tokio::fs::canonicalize(&path_str).await.map_err(|e| {
                    ctx.runtime_error(format!("Io.Directory.canonicalize: {e}"))
                })?;
            let sid = ctx.vals().intern(&canonical.to_string_lossy());

            Ok(ctx.vals().add(Payload::FilePath(sid)))
        })
    }

    /// `(FilePath) -> Option[FilePath]`
    ///
    /// Returns the parent directory of a path.
    pub(crate) fn parent(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let path_str = Self::path_str(ctx, a, "Io.Directory.parent")?;
        let path = Path::new(&path_str);

        match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => {
                let sid = ctx.vals().intern(&p.to_string_lossy());
                let fp = ctx.vals().add(Payload::FilePath(sid));
                Ok(ctx.vals().option_some(fp))
            }
            _ => Ok(ctx.vals().option_none()),
        }
    }

    /// `(FilePath) -> Option[String]`
    ///
    /// Returns the final component of a path.
    pub(crate) fn file_name(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let path_str = Self::path_str(ctx, a, "Io.Directory.file-name")?;
        let name = Path::new(&path_str)
            .file_name()
            .map(|n| n.to_string_lossy().to_string());

        Self::option_string(ctx, name)
    }

    /// `(FilePath) -> Option[String]`
    ///
    /// Returns the file extension, if any.
    pub(crate) fn extension(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let path_str = Self::path_str(ctx, a, "Io.Directory.extension")?;
        let ext = Path::new(&path_str)
            .extension()
            .map(|e| e.to_string_lossy().to_string());

        Self::option_string(ctx, ext)
    }

    /// `(FilePath, Array[String]) -> FilePath`
    ///
    /// Joins path components.
    pub(crate) fn join(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let b = args[1];
        let base_str = Self::path_str(ctx, a, "Io.Directory.join")?;
        let parts: SmallVec<[ValueId; 4]> = ctx
            .vals()
            .array(b, "Io.Directory.join")?
            .iter()
            .copied()
            .collect();
        let part_strs: SmallVec<[String; 4]> = parts
            .into_iter()
            .map(|id| Self::string(ctx, id, "Io.Directory.join"))
            .collect::<Result<_>>()?;
        let mut path = PathBuf::from(&base_str);

        part_strs.iter().for_each(|p| path.push(p));
        let sid = ctx.vals().intern(&path.to_string_lossy());
        Ok(ctx.vals().add(Payload::FilePath(sid)))
    }

    /// `() -> FilePath`
    ///
    /// Returns the system temporary directory.
    pub(crate) fn temp_dir(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        _args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let tmp = env::temp_dir();
        let sid = ctx.vals().intern(&tmp.to_string_lossy());

        Ok(ctx.vals().add(Payload::FilePath(sid)))
    }

    /// `(FilePath, String) -> FilePath`
    ///
    /// Returns a new path with the given extension.
    pub(crate) fn with_extension(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        let b = args[1];
        let path_str = Self::path_str(ctx, a, "Io.Directory.with-extension")?;
        let ext = Self::string(ctx, b, "Io.Directory.with-extension")?;
        let mut path = PathBuf::from(&path_str);

        path.set_extension(&ext);
        let sid = ctx.vals().intern(&path.to_string_lossy());
        Ok(ctx.vals().add(Payload::FilePath(sid)))
    }

    fn path_str(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        id: ValueId,
        label: &str,
    ) -> Result<String> {
        let sid = match ctx.vals().payload(id)? {
            Payload::FilePath(sid) => *sid,
            _ => typechecked!(label, "FilePath"),
        };
        Ok(ctx.vals().str(sid)?.to_owned())
    }

    fn string(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        id: ValueId,
        label: &str,
    ) -> Result<String> {
        let sid = ctx.vals().string_id(id, label)?;
        Ok(ctx.vals().str(sid)?.to_owned())
    }

    fn make_path_file(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        path_str: &str,
    ) -> ValueId {
        Self::make_path(ctx, path_str, 0)
    }

    fn make_path_dir(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        path_str: &str,
    ) -> ValueId {
        Self::make_path(ctx, path_str, 1)
    }

    fn make_path(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        path_str: &str,
        tag: u8,
    ) -> ValueId {
        let sid = ctx.vals().intern(path_str);
        let fp = ctx.vals().add(Payload::FilePath(sid));
        let path_ty = ctx.vals().type_id(TypeId::PATH);
        ctx.vals().add_typed(
            Payload::Variant {
                tag,
                vals: smallvec![fp],
            },
            path_ty,
        )
    }

    fn object_fields(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        id: ValueId,
        label: &str,
    ) -> Result<IndexMap<StringId, ValueId>> {
        match ctx.vals().payload(id)? {
            Payload::Object(fields) => Ok(fields.as_ref().clone()),
            _ => typechecked!(label, "Object"),
        }
    }

    fn object_pair(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        id: ValueId,
        label: &str,
    ) -> Result<(ValueId, ValueId)> {
        let fields = Self::object_fields(ctx, id, label)?;
        let l = fields.values().next().copied().ok_or_else(|| {
            ctx.runtime_error(format!("{label}: missing first field"))
        })?;
        let r = fields.values().nth(1).copied().ok_or_else(|| {
            ctx.runtime_error(format!("{label}: missing second field"))
        })?;

        Ok((l, r))
    }

    fn path_pair(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        id: ValueId,
        label: &str,
    ) -> Result<(String, String)> {
        let (l, r) = Self::object_pair(ctx, id, label)?;
        Ok((
            Self::path_str(ctx, l, label)?,
            Self::path_str(ctx, r, label)?,
        ))
    }

    fn path_contents(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        id: ValueId,
        label: &str,
    ) -> Result<(String, String)> {
        let (path, contents) = Self::object_pair(ctx, id, label)?;
        Ok((
            Self::path_str(ctx, path, label)?,
            Self::string(ctx, contents, label)?,
        ))
    }

    fn string_pair(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        id: ValueId,
        label: &str,
    ) -> Result<(String, String)> {
        let (l, r) = Self::object_pair(ctx, id, label)?;
        Ok((Self::string(ctx, l, label)?, Self::string(ctx, r, label)?))
    }

    fn option_string(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        val: Option<String>,
    ) -> Result<ValueId> {
        Ok(match val {
            Some(s) => {
                let sid = ctx.vals().intern(&s);
                let id = ctx.vals().add(Payload::String(sid));
                ctx.vals().option_some(id)
            }
            None => ctx.vals().option_none(),
        })
    }
}
