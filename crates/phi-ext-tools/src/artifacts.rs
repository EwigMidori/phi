//! Immutable execution outputs. The host owns storage and project association.
use async_trait::async_trait;
use phi_kernel::{ImageId, SessionId};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path};

pub const MAX_ARTIFACT_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_ARTIFACTS: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ArtifactId(String);
impl ArtifactId {
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for ArtifactId {
    type Error = String;
    fn try_from(value: String) -> Result<Self, String> {
        uuid::Uuid::parse_str(&value).map_err(|e| e.to_string())?;
        Ok(Self(value))
    }
}
impl From<ArtifactId> for String {
    fn from(id: ArtifactId) -> Self {
        id.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArtifactMetadata {
    pub id: ArtifactId,
    pub name: String,
    pub mime_type: String,
    pub byte_len: u64,
    pub image_id: Option<ImageId>,
}
impl ArtifactMetadata {
    pub fn from_output(output: &serde_json::Value) -> Result<Vec<Self>, String> {
        match output.get("artifacts") {
            Some(value) => serde_json::from_value(value.clone()).map_err(|e| e.to_string()),
            None => Ok(Vec::new()),
        }
    }
}

pub struct ArtifactFile {
    pub name: String,
    pub bytes: Vec<u8>,
}

#[async_trait]
pub trait ExecutionArtifacts: Send + Sync {
    async fn publish(
        &self,
        session: &SessionId,
        files: Vec<ArtifactFile>,
    ) -> Result<Vec<ArtifactMetadata>, String>;
    async fn read(&self, session: &SessionId, id: &ArtifactId) -> Result<Vec<u8>, String>;
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ArtifactInput {
    pub id: ArtifactId,
    pub path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Publication {
    path: String,
    name: String,
}

pub(crate) struct ArtifactWorkspace;
impl ArtifactWorkspace {
    fn publication_error(path: &str, error: &std::io::Error) -> String {
        if error.kind() == std::io::ErrorKind::NotFound {
            format!(
                "Cannot publish {path:?}: file does not exist. Save the file before artifacts.publish() in the same run_python call and keep it until execution finishes. publish() does not create or save files."
            )
        } else {
            format!("Cannot publish {path:?}: {error}")
        }
    }

    pub fn relative(path: &str) -> Result<&Path, String> {
        let path = Path::new(path);
        if path.as_os_str().is_empty()
            || !path.components().all(|c| matches!(c, Component::Normal(_)))
        {
            return Err("Artifact paths must be nonempty relative paths without traversal".into());
        }
        Ok(path)
    }
    pub fn collect(root: &Path) -> Result<Vec<ArtifactFile>, String> {
        let manifest = root.join(".artifacts.json");
        // The bootstrap always writes a manifest on success, including zero outputs.
        let metadata = std::fs::symlink_metadata(&manifest)
            .map_err(|e| format!("Cannot read artifact manifest .artifacts.json: {e}"))?;
        if !metadata.file_type().is_file() || metadata.len() > 64 * 1024 {
            return Err("Invalid artifact manifest".into());
        }
        let entries: Vec<Publication> = serde_json::from_slice(
            &std::fs::read(manifest)
                .map_err(|e| format!("Cannot read artifact manifest .artifacts.json: {e}"))?,
        )
        .map_err(|e| format!("Invalid artifact manifest .artifacts.json: {e}"))?;
        if entries.len() > MAX_ARTIFACTS {
            return Err("Too many artifacts (maximum 16)".into());
        }
        let root = root
            .canonicalize()
            .map_err(|e| format!("Cannot access artifact execution directory: {e}"))?;
        let mut total = 0usize;
        let mut files = Vec::new();
        for entry in entries {
            Self::relative(&entry.path)?;
            if entry.name.trim().is_empty()
                || entry.name.len() > 240
                || entry.name.contains(['/', '\\'])
            {
                return Err("Invalid artifact name".into());
            }
            let path = root
                .join(&entry.path)
                .canonicalize()
                .map_err(|e| Self::publication_error(&entry.path, &e))?;
            if !path.starts_with(&root) {
                return Err(format!(
                    "Artifact {:?} is outside the execution directory",
                    entry.path
                ));
            }
            let metadata = path
                .metadata()
                .map_err(|e| Self::publication_error(&entry.path, &e))?;
            if !metadata.is_file()
                || metadata.len() == 0
                || metadata.len() > MAX_ARTIFACT_BYTES as u64
            {
                return Err(format!(
                    "Artifact {:?} must be a nonempty file of at most 32 MiB",
                    entry.path
                ));
            }
            total = total.saturating_add(metadata.len() as usize);
            if total > MAX_ARTIFACT_BYTES {
                return Err("Artifacts exceed the 32 MiB execution limit".into());
            }
            let bytes =
                std::fs::read(&path).map_err(|e| Self::publication_error(&entry.path, &e))?;
            if bytes.len() as u64 != metadata.len() {
                return Err(format!(
                    "Artifact {:?} changed during collection",
                    entry.path
                ));
            }
            files.push(ArtifactFile {
                name: entry.name,
                bytes,
            });
        }
        Ok(files)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn collection_reads_only_declared_files_and_rejects_escape_and_oversize() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("data.csv"), b"x,y\n1,2").unwrap();
        std::fs::write(root.path().join("private.txt"), b"not published").unwrap();
        let manifest = root.path().join(".artifacts.json");
        std::fs::write(&manifest, r#"[{"path":"data.csv","name":"data.csv"}]"#).unwrap();
        let files = ArtifactWorkspace::collect(root.path()).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].bytes, b"x,y\n1,2");
        std::fs::write(
            &manifest,
            r#"[{"path":"../outside.csv","name":"data.csv"}]"#,
        )
        .unwrap();
        assert!(ArtifactWorkspace::collect(root.path()).is_err());
        let large = std::fs::File::create(root.path().join("large.csv")).unwrap();
        large.set_len(MAX_ARTIFACT_BYTES as u64 + 1).unwrap();
        std::fs::write(&manifest, r#"[{"path":"large.csv","name":"large.csv"}]"#).unwrap();
        assert!(ArtifactWorkspace::collect(root.path()).is_err());
        std::fs::write(
            &manifest,
            "[".to_owned() + &vec![r#"{"path":"data.csv","name":"data.csv"}"#; 17].join(",") + "]",
        )
        .unwrap();
        assert!(ArtifactWorkspace::collect(root.path()).is_err());
    }
}
