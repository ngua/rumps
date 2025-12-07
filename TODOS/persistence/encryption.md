# Encryption at Rest

## Overview

This document outlines the design considerations for adding encryption at rest to RUMPS storage.

**Difficulty assessment**: Medium - the architecture has good abstraction points, but multiple serialization boundaries require careful handling.

---

## Architecture Summary

### What makes encryption tractable

1. **Clean abstraction layer**: The `AsyncStorageEngine` trait (`engine.rs:52-98`) provides a natural interception point - wrap `FileStorageEngine` with an encrypting layer

2. **Centralized I/O**: Page writes funnel through `write_page_at()` and `write_page_to_disk()` (`engine/file.rs:851-887`) - single place for encryption hooks

3. **Fixed page size**: All data pages are `PAGE_SIZE` bytes (`4096` default), works well with block ciphers like AES

### Complexity factors

1. **Dual storage paths**: Both data file (`data.db`) and WAL files need separate encryption
   - Data: page-aligned, straightforward
   - WAL: variable-size records with checksums (`wal/format.rs:133-194`)

2. **Multiple serialization formats**:
   - Nodes: `bincode`
   - Superblock/MetadataPage/Registry: custom binary formats
   - Each needs encryption consideration

3. **Bootstrap problem**: Superblock (page 0) cannot be encrypted - needed to locate everything else. MetadataPage may need to stay readable to determine *if* DB is encrypted.

---

## Recommended Implementation

### Data File Encryption

Create an `EncryptedStorageEngine` wrapper:

```rust
pub(crate) struct EncryptedStorageEngine<C: Cipher> {
    inner: Arc<FileStorageEngine>,
    cipher: C,
}

#[async_trait]
impl<C: Cipher + Send + Sync> AsyncStorageEngine for EncryptedStorageEngine<C> {
    async fn read(&self, id: NodeId) -> Result<Node> {
        // Inner engine handles cache; encryption at disk boundary
        self.inner.read(id).await
    }

    async fn flush(&self) -> Result<()> {
        // Intercept: encrypt serialized bytes before disk write
        // ...
    }
}
```

**Key points**:
- `PageCache` holds unencrypted `Node` objects in memory (correct - fast access)
- Encrypt after serialization, decrypt before deserialization
- Use AES-GCM or similar AEAD for authenticated encryption

### WAL Encryption

Modify `wal/writer.rs` to encrypt each `WalRecord` payload:

```rust
// In WalWriter::append()
let payload = bincode::serialize(&record)?;
let encrypted = self.cipher.encrypt(&payload)?;
// Write encrypted payload with updated length in RecordHeader
```

The `RecordHeader` (`wal/format.rs:133-194`) already includes a CRC32 checksum - consider whether to:
- Checksum before encryption (detect corruption before decrypt)
- Checksum after encryption (standard AEAD approach)

### What NOT to encrypt

| Structure           | Encrypt? | Reason                                            |
|---------------------|----------|---------------------------------------------------|
| Superblock (page 0) | No       | Bootstrap - must read to find metadata            |
| MetadataPage        | Partial  | Version/magic readable; config could be encrypted |
| Registry            | Yes      | Contains global names                             |
| Data pages          | Yes      | User data                                         |
| WAL records         | Yes      | Contains values                                   |

---

## Key Management

Key management is explicitly **out of scope** for the storage layer. The API should accept keys/ciphers from external sources:

```rust
// Option 1: Accept cipher instance
Database::builder()
    .encryption(AesGcmCipher::new(key))
    .create("./data")
    .await?;

// Option 2: Accept key, construct cipher internally
Database::builder()
    .encryption_key(&key_bytes)
    .create("./data")
    .await?;
```

External systems handle:
- Key derivation (from passphrase, KMS, HSM, etc.)
- Key rotation
- Key storage

---

## Format Versioning

The `MetadataPage` version field must distinguish encrypted vs unencrypted databases:

```rust
// Proposed: use high bit or separate field
const VERSION_ENCRYPTED: u32 = 0x8000_0000;

// Or explicit field in MetadataPage
pub struct MetadataPage {
    // ...
    pub encrypted: bool,
    pub encryption_algo: Option<EncryptionAlgorithm>,
}
```

---

## Performance Considerations

1. **Hardware acceleration**: Use `aes` crate with `aes-ni` feature for hardware AES
2. **Async compatibility**: Crypto is CPU-bound; consider `spawn_blocking` for large operations
3. **Page-level granularity**: Encrypt/decrypt entire pages, not individual fields

---

## Estimated Effort

| Task                             | Effort   |
|----------------------------------|----------|
| `EncryptedStorageEngine` wrapper | 2-3 days |
| WAL encryption                   | 3-5 days |
| Metadata/format changes          | 1-2 days |
| Test coverage                    | 3-4 days |
| **Total**                        | ~2 weeks |
