use std::{path::PathBuf, process::Command};
use uuid::Uuid;

struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn server_requires_direct_sdk_networking_before_loading_credentials_or_opening_listeners() {
    let root = Temp(std::env::temp_dir().join(format!("ct-startup-{}", Uuid::new_v4())));
    std::fs::create_dir_all(&root.0).unwrap();
    let mut config: serde_yaml::Value =
        serde_yaml::from_str(include_str!("../config.example.yaml")).unwrap();
    config["worker_addr"] = serde_yaml::Value::String("127.0.0.1:8091".into());
    config["data_dir"] = serde_yaml::Value::String(root.0.join("data").to_string_lossy().into());
    let path = root.0.join("config.yaml");
    std::fs::write(&path, serde_yaml::to_string(&config).unwrap()).unwrap();
    for bypass in [None, Some("localhost,127.0.0.1")] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_control-tower"));
        command
            .args(["--config", path.to_str().unwrap(), "serve"])
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .env(
                "GOOGLE_APPLICATION_CREDENTIALS",
                root.0.join("does-not-exist.json"),
            );
        if let Some(bypass) = bypass {
            command.env("NO_PROXY", bypass);
        }
        let output = command.output().unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("set NO_PROXY=*"));
        assert!(!root.0.join("data").exists());
    }
}
