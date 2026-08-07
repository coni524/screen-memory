//! Process-wide shared tokio runtime, used for AWS SDK calls.

use std::sync::OnceLock;

use tokio::runtime::Runtime;

pub fn runtime() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| Runtime::new().expect("cannot create the tokio runtime"))
}
