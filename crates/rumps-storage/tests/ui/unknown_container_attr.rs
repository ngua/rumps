use rumps_storage::orm::ToRumps;

#[derive(ToRumps)]
#[rumps(global = "test", foobar)]
struct UnknownContainerAttr {
    #[rumps(key)]
    id: u64,
    name: String,
}

fn main() {}
