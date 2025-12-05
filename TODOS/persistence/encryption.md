# Encryption at Rest

## Overview

This document outlines the design considerations for adding **encryption at rest** to RUMPS storage.

**Scope**: This design protects data *on disk* — it does **not** protect data in use. While the database is open:
- Decrypted data lives in memory (page cache, WAL buffers, application structs)
- Encryption keys reside in process memory
- An attacker with access to the running process can read plaintext data directly

**What this protects against**:
- Stolen or decommissioned storage media
- Unauthorized filesystem access (backup tapes, snapshots, cloud storage)
- Forensic recovery from discarded disks

**What this does NOT protect against**:
- Attackers with root/admin access to the running system
- Memory disclosure vulnerabilities in the application
- Physical access to a running machine

**Difficulty assessment**: Medium — the architecture has good abstraction points, but multiple serialization boundaries require careful handling.

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

## Security Considerations

### Threat Model

This encryption scheme assumes:
- The attacker does **not** have access to the running RUMPS process
- The attacker has access to the storage medium (disk, backup, snapshot)
- The key is securely managed outside the storage layer

If an attacker can attach a debugger, read `/proc/[pid]/mem`, or exploit a memory bug, they can extract keys or read decrypted data directly. This is inherent to any in-process encryption — mitigations exist but cannot eliminate the risk entirely.

### Memory Attack Vectors

| Vector            | Description                                   | Mitigation                                  |
|-------------------|-----------------------------------------------|---------------------------------------------|
| Core dumps        | Key appears in crash dump files               | `MADV_DONTDUMP` / disable core dumps        |
| Swap/hibernation  | Key written to disk with swapped pages        | `mlock()` to pin key in RAM                 |
| `/proc/[pid]/mem` | Privileged process reads memory               | OS hardening, reduced privileges            |
| Cold boot         | RAM contents persist briefly after power loss | Physical security; limited mitigation       |
| Spectre-class     | Side-channel extraction                       | CPU mitigations; limited in-process defense |
| Heap bugs         | Use-after-free / overflow leaking key         | `zeroize` on drop; careful memory handling  |

### Recommended Key Handling

Use the `secrecy` and `zeroize` crates to reduce exposure:

```rust
use secrecy::{ExposeSecret, Secret};
use zeroize::Zeroizing;

pub struct EncryptedStorageEngine<C: Cipher> {
    inner: Arc<FileStorageEngine>,
    // Key wrapped in Secret — zeroizes on drop, won't appear in Debug/Display
    key: Secret<Zeroizing<[u8; 32]>>,
    cipher: C,
}

impl<C: Cipher> EncryptedStorageEngine<C> {
    fn encrypt(&self, data: &[u8]) -> Result<Vec<u8>> {
        // Expose key only when needed, briefly
        self.cipher.encrypt(self.key.expose_secret(), data)
    }
}
```

### Optional Hardening: `mlock`

Prevent the key from being swapped to disk:

```rust
use memsec::mlock;

// During initialization
let key_buf: [u8; 32] = derive_key(...);
unsafe {
    // Pin in RAM — may fail if ulimit restricts locked memory
    if mlock(key_buf.as_ptr(), key_buf.len()).is_err() {
        // Log warning; continue without mlock (still functional, less secure)
    }
}
```

**Note**: `mlock` requires sufficient `RLIMIT_MEMLOCK`. Consider documenting this for deployments requiring hardened key handling.

### Recommended Crates

| Crate     | Purpose                                                   | Recommendation                                            |
|-----------|-----------------------------------------------------------|-----------------------------------------------------------|
| `secrecy` | Wraps secrets; zeroizes on drop; blocks `Debug`/`Display` | **Use** — lightweight, well-maintained                    |
| `zeroize` | Trait for secure memory zeroing                           | **Use** — dependency of `secrecy`, also useful standalone |
| `memsec`  | `mlock`, `mprotect`, secure allocators                    | **Optional** — use for `mlock` hardening if needed        |
| `secrets` | Alternative to `secrecy` with `mlock` built-in            | Consider if `memsec` is too low-level                     |

**Recommendation**: Use `secrecy` + `zeroize` as the baseline. Add `memsec::mlock` for deployments requiring swap protection. Avoid rolling custom solutions.

```toml
# Cargo.toml
[dependencies]
secrecy = { version = "0.8", features = ["zeroize"] }
zeroize = { version = "1", features = ["derive"] }
memsec = "0.7"  # Optional, for mlock
```

### What We Explicitly Do NOT Provide

- **Key management**: External responsibility (KMS, HSM, passphrase derivation)
- **Key rotation**: Application must handle re-encryption
- **Memory encryption**: Keys and plaintext exist in process memory
- **Tamper evidence**: AEAD provides integrity, not tamper logging
- **Secure enclaves**: No SGX/TrustZone integration (future consideration?)

---

## Performance Considerations

1. **Hardware acceleration**: Use `aes` crate with `aes-ni` feature for hardware AES
2. **Async compatibility**: Crypto is CPU-bound; consider `spawn_blocking` for large operations
3. **Page-level granularity**: Encrypt/decrypt entire pages, not individual fields

---

## Estimated Effort

| Task                             | Effort     |
|----------------------------------|------------|
| `EncryptedStorageEngine` wrapper | 2-3 days   |
| WAL encryption                   | 3-5 days   |
| Metadata/format changes          | 1-2 days   |
| Key handling (`secrecy`/`mlock`) | 1-2 days   |
| Test coverage                    | 3-4 days   |
| Security documentation           | 0.5 days   |
| **Total**                        | ~2.5 weeks |
