use rumps_storage::orm::ToRumps;

#[derive(ToRumps)]
#[rumps(global = "test")]
struct UnknownFieldAttr {
    #[rumps(key)]
    id: u64,
    #[rumps(foobar)]
    name: String,
}

fn main() {}
