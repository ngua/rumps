use rumps_storage::orm::ToRumps;

#[derive(ToRumps)]
#[rumps(global = "test", rename_all = "invalid-case")]
struct InvalidRenameAll {
    #[rumps(key)]
    id: u64,
    name: String,
}

fn main() {}
