use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::domain::AgentTarget;
use crate::patch::{apply_patch, atomic_write, read_before_checked, ConfigPatch, PatchOptions};

pub const SKILL_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SkillSyncMethod {
    Symlink,
    Copy,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillTargets {
    #[serde(default)]
    pub opencode: bool,
    #[serde(default)]
    pub claude: bool,
    #[serde(default)]
    pub codex: bool,
}

impl SkillTargets {
    pub fn get(&self, target: AgentTarget) -> bool {
        match target {
            AgentTarget::OpenCode => self.opencode,
            AgentTarget::ClaudeCode => self.claude,
            AgentTarget::Codex => self.codex,
            AgentTarget::Other => false,
        }
    }

    pub fn set(&mut self, target: AgentTarget, enabled: bool) {
        match target {
            AgentTarget::OpenCode => self.opencode = enabled,
            AgentTarget::ClaudeCode => self.claude = enabled,
            AgentTarget::Codex => self.codex = enabled,
            AgentTarget::Other => {}
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase", tag = "kind")]
pub enum SkillOrigin {
    Local {
        source: String,
    },
    Zip {
        source: String,
    },
    GitHub {
        owner: String,
        repo: String,
        #[serde(default = "default_branch")]
        branch: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subdir: Option<String>,
    },
}

pub struct GitHubSkillSource<'a> {
    pub owner: &'a str,
    pub repo: &'a str,
    pub branch: &'a str,
    pub subdir: Option<&'a str>,
}

fn default_branch() -> String {
    "main".to_string()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillBackupRecord {
    pub id: String,
    pub skill_id: String,
    pub created_at: String,
    pub path: String,
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<SkillOrigin>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillRecord {
    pub id: String,
    pub name: String,
    pub path: String,
    pub sha256: String,
    pub sync_method: SkillSyncMethod,
    #[serde(default)]
    pub targets: SkillTargets,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<SkillOrigin>,
}

impl SkillRecord {
    pub fn validate(&self) -> Result<(), String> {
        validate_id(&self.id)?;
        if self.name.trim().is_empty() {
            return Err("skill name cannot be empty".to_string());
        }
        if self.sha256.len() != 64 || !self.sha256.chars().all(|ch| ch.is_ascii_hexdigit()) {
            return Err(format!("skill {} has invalid SHA-256", self.id));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillStore {
    pub schema_version: u32,
    #[serde(default)]
    pub skills: BTreeMap<String, SkillRecord>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectionState {
    Absent,
    Present,
}

#[derive(Clone, Debug)]
pub struct ProjectionPatch {
    pub id: String,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub method: SkillSyncMethod,
    pub before: ProjectionState,
    pub after: ProjectionState,
}

#[derive(Clone, Debug)]
pub enum AppliedProjection {
    Created(ProjectionPatch),
    Quarantined {
        patch: ProjectionPatch,
        quarantine: PathBuf,
    },
}

impl Default for SkillStore {
    fn default() -> Self {
        Self {
            schema_version: SKILL_SCHEMA_VERSION,
            skills: BTreeMap::new(),
        }
    }
}

pub fn store_path(home: &Path) -> PathBuf {
    home.join(".codex/xu-skills.json")
}

pub fn source_root(home: &Path) -> PathBuf {
    home.join(".codex/xu-skills")
}

pub fn app_root(home: &Path, target: AgentTarget) -> Result<PathBuf, String> {
    match target {
        AgentTarget::OpenCode => Ok(home.join(".config/opencode/skills")),
        AgentTarget::ClaudeCode => Ok(home.join(".claude/skills")),
        AgentTarget::Codex => Ok(home.join(".codex/skills")),
        AgentTarget::Other => Err("unsupported skill target".to_string()),
    }
}

pub fn read_store(home: &Path) -> Result<SkillStore, String> {
    let path = store_path(home);
    let text = read_before_checked(&path)?;
    if text.trim().is_empty() {
        return Ok(SkillStore::default());
    }
    let store: SkillStore = serde_json::from_str(&text)
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    if store.schema_version != SKILL_SCHEMA_VERSION {
        return Err(format!(
            "unsupported skill schema {}, expected {}",
            store.schema_version, SKILL_SCHEMA_VERSION
        ));
    }
    for (id, skill) in &store.skills {
        if id != &skill.id {
            return Err(format!(
                "skill map id {id} does not match record id {}",
                skill.id
            ));
        }
        skill.validate()?;
    }
    Ok(store)
}

pub fn store_patch(home: &Path, store: &SkillStore) -> Result<ConfigPatch, String> {
    for skill in store.skills.values() {
        skill.validate()?;
    }
    let path = store_path(home);
    let before = read_before_checked(&path)?;
    let after = serde_json::to_string_pretty(store).map_err(|error| error.to_string())? + "\n";
    Ok(ConfigPatch::new(path, before, after))
}

pub fn import_local(
    home: &Path,
    id: &str,
    name: &str,
    source: &Path,
    method: SkillSyncMethod,
    dry_run: bool,
) -> Result<String, String> {
    validate_id(id)?;
    let source = source
        .canonicalize()
        .map_err(|error| format!("resolve {}: {error}", source.display()))?;
    validate_skill_dir(&source)?;
    let hash = hash_directory(&source)?;
    let mut store = read_store(home)?;
    if store.skills.contains_key(id) {
        return Err(format!("skill already exists: {id}"));
    }
    let destination = source_root(home).join(id);
    if destination.exists() {
        return Err(format!(
            "skill destination already exists: {}",
            destination.display()
        ));
    }
    let record = SkillRecord {
        id: id.to_string(),
        name: name.to_string(),
        path: destination.display().to_string(),
        sha256: hash,
        sync_method: method,
        targets: SkillTargets::default(),
        description: skill_description(&source)?,
        origin: Some(SkillOrigin::Local {
            source: source.display().to_string(),
        }),
    };
    record.validate()?;
    store.skills.insert(id.to_string(), record);
    let patch = store_patch(home, &store)?;
    if dry_run {
        let result = apply_patch(
            &patch,
            PatchOptions {
                dry_run: true,
                backup: true,
            },
        )
        .map_err(|error| error.to_string())?;
        return Ok(format!(
            "Dry-run skill import\nsource {}\ndestination {}\n{}",
            source.display(),
            destination.display(),
            result.diff
        ));
    }
    let temp = prepare_directory_copy(&source, &destination)?;
    fs::rename(&temp, &destination).map_err(|error| format!("install skill: {error}"))?;
    if let Err(error) = apply_patch(
        &patch,
        PatchOptions {
            dry_run: false,
            backup: true,
        },
    ) {
        let _ = fs::remove_dir_all(&destination);
        return Err(error.to_string());
    }
    Ok(format!("Imported skill {id}\n{}", destination.display()))
}

pub fn backup_root(home: &Path) -> PathBuf {
    home.join(".codex/xu-skill-backups")
}

pub fn backup_index_path(home: &Path) -> PathBuf {
    home.join(".codex/xu-skill-backups.json")
}

pub fn read_backup_index(home: &Path) -> Result<Vec<SkillBackupRecord>, String> {
    let path = backup_index_path(home);
    let text = read_before_checked(&path)?;
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn write_backup_index(home: &Path, records: &[SkillBackupRecord]) -> Result<(), String> {
    let path = backup_index_path(home);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    let after = serde_json::to_string_pretty(records).map_err(|error| error.to_string())? + "\n";
    let before = read_before_checked(&path)?;
    apply_patch(
        &ConfigPatch::new(path, before, after),
        PatchOptions {
            dry_run: false,
            backup: true,
        },
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

pub fn install_zip(
    home: &Path,
    id: &str,
    name: &str,
    zip_path: &Path,
    method: SkillSyncMethod,
    dry_run: bool,
) -> Result<String, String> {
    validate_id(id)?;
    let extract_root = staging_root(home).join(format!("zip-{id}-{}", std::process::id()));
    if extract_root.exists() {
        fs::remove_dir_all(&extract_root)
            .map_err(|error| format!("remove stale zip stage: {error}"))?;
    }
    extract_zip(zip_path, &extract_root)?;
    let skill_dir = find_skill_root(&extract_root)?;
    let result = import_from_prepared(
        home,
        id,
        name,
        &skill_dir,
        method,
        SkillOrigin::Zip {
            source: zip_path.display().to_string(),
        },
        dry_run,
    );
    let _ = fs::remove_dir_all(&extract_root);
    result
}

pub fn install_github(
    home: &Path,
    id: &str,
    name: &str,
    source: GitHubSkillSource<'_>,
    method: SkillSyncMethod,
    dry_run: bool,
) -> Result<String, String> {
    validate_id(id)?;
    let clone_root = staging_root(home).join(format!("git-{id}-{}", std::process::id()));
    if clone_root.exists() {
        fs::remove_dir_all(&clone_root)
            .map_err(|error| format!("remove stale git stage: {error}"))?;
    }
    clone_github_repo(source.owner, source.repo, source.branch, &clone_root)?;
    let skill_dir = match source.subdir {
        Some(value) if !value.trim().is_empty() => {
            let path = resolve_skill_subdir(&clone_root, value)?;
            validate_skill_dir(&path)?;
            path
        }
        _ => find_skill_root(&clone_root)?,
    };
    let result = import_from_prepared(
        home,
        id,
        name,
        &skill_dir,
        method,
        SkillOrigin::GitHub {
            owner: source.owner.to_string(),
            repo: source.repo.to_string(),
            branch: source.branch.to_string(),
            subdir: source
                .subdir
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
        },
        dry_run,
    );
    let _ = fs::remove_dir_all(&clone_root);
    result
}

pub fn update_skill(home: &Path, id: &str, dry_run: bool) -> Result<String, String> {
    let store = read_store(home)?;
    let skill = store
        .skills
        .get(id)
        .ok_or_else(|| format!("unknown skill: {id}"))?
        .clone();
    let origin = skill
        .origin
        .clone()
        .ok_or_else(|| format!("skill {id} has no update origin"))?;
    let current_hash = hash_directory(Path::new(&skill.path))?;
    let stage = fetch_origin_stage(home, id, &origin)?;
    let next_hash = hash_directory(&stage)?;
    if next_hash == current_hash && next_hash == skill.sha256 {
        let _ = fs::remove_dir_all(stage.parent().unwrap_or(Path::new(".")));
        return Ok(format!(
            "Skill {id} is already up to date ({})",
            &current_hash[..12]
        ));
    }
    if dry_run {
        let message = format!(
            "Dry-run skill update\nid {id}\ncurrent {}\nnext {}\norigin {:?}",
            &current_hash[..12],
            &next_hash[..12],
            origin
        );
        let _ = fs::remove_dir_all(stage.parent().unwrap_or(Path::new(".")));
        return Ok(message);
    }
    let backup = create_skill_backup(home, &skill)?;
    let destination = PathBuf::from(&skill.path);
    let parent = destination
        .parent()
        .ok_or_else(|| "skill destination has no parent".to_string())?;
    let replaced = parent.join(format!(".{}.xu-update-old-{}", id, std::process::id()));
    let copy_targets = [
        AgentTarget::OpenCode,
        AgentTarget::ClaudeCode,
        AgentTarget::Codex,
    ]
    .into_iter()
    .filter(|target| skill.sync_method == SkillSyncMethod::Copy && skill.targets.get(*target))
    .map(|target| {
        let projection = app_root(home, target)?.join(id);
        inspect_projection(&destination, &projection, id)?;
        Ok(projection)
    })
    .collect::<Result<Vec<_>, String>>()?;
    if destination.exists() {
        fs::rename(&destination, &replaced)
            .map_err(|error| format!("quarantine old skill: {error}"))?;
    }
    if let Err(error) = fs::rename(&stage, &destination) {
        if replaced.exists() {
            let _ = fs::rename(&replaced, &destination);
        }
        let _ = fs::remove_dir_all(stage.parent().unwrap_or(Path::new(".")));
        return Err(format!("install updated skill: {error}"));
    }
    let mut refreshed = Vec::new();
    for projection in &copy_targets {
        let quarantine = quarantine_path(projection);
        let refresh = fs::rename(projection, &quarantine)
            .map_err(|error| format!("quarantine {}: {error}", projection.display()))
            .and_then(|()| {
                create_projection(&destination, projection, SkillSyncMethod::Copy, id).inspect_err(
                    |_| {
                        let _ = fs::rename(&quarantine, projection);
                    },
                )
            });
        if let Err(error) = refresh {
            rollback_refreshed_projections(&destination, id, &refreshed);
            let _ = fs::remove_dir_all(&destination);
            if replaced.exists() {
                let _ = fs::rename(&replaced, &destination);
            }
            let _ = fs::remove_dir_all(stage.parent().unwrap_or(Path::new(".")));
            return Err(format!("refresh skill projection: {error}"));
        }
        refreshed.push((projection.clone(), quarantine));
    }
    let mut store = read_store(home)?;
    if let Some(record) = store.skills.get_mut(id) {
        record.sha256 = next_hash.clone();
        record.description = skill_description(&destination)?;
        record.origin = Some(origin);
    }
    if let Err(error) = apply_patch(
        &store_patch(home, &store)?,
        PatchOptions {
            dry_run: false,
            backup: true,
        },
    ) {
        rollback_refreshed_projections(&destination, id, &refreshed);
        let _ = fs::remove_dir_all(&destination);
        if replaced.exists() {
            let _ = fs::rename(&replaced, &destination);
        }
        return Err(error.to_string());
    }
    for (_, quarantine) in refreshed {
        let _ = fs::remove_dir_all(quarantine);
    }
    let _ = fs::remove_dir_all(&replaced);
    let _ = fs::remove_dir_all(stage.parent().unwrap_or(Path::new(".")));
    Ok(format!(
        "Updated skill {id}\nbackup {}\nsha256 {}",
        backup.id,
        &next_hash[..12]
    ))
}

pub fn uninstall_skill(home: &Path, id: &str, dry_run: bool) -> Result<String, String> {
    let mut store = read_store(home)?;
    let skill = store
        .skills
        .get(id)
        .ok_or_else(|| format!("unknown skill: {id}"))?
        .clone();
    if dry_run {
        return Ok(format!(
            "Dry-run skill uninstall\nid {id}\npath {}\nenabled {:?}",
            skill.path, skill.targets
        ));
    }
    let backup = create_skill_backup(home, &skill)?;
    let mut removed = Vec::new();
    for target in [
        AgentTarget::OpenCode,
        AgentTarget::ClaudeCode,
        AgentTarget::Codex,
    ] {
        if skill.targets.get(target) {
            let patch = projection_patch(home, &skill, target, false)?;
            match apply_projection_patch(&patch) {
                Ok(Some(applied)) => removed.push(applied),
                Ok(None) => {}
                Err(error) => {
                    let rollback = rollback_applied_projections(&removed);
                    return Err(with_rollback_error(error, rollback));
                }
            }
        }
    }
    store.skills.remove(id);
    if let Err(error) = apply_patch(
        &store_patch(home, &store)?,
        PatchOptions {
            dry_run: false,
            backup: true,
        },
    ) {
        let rollback = rollback_applied_projections(&removed);
        return Err(with_rollback_error(error.to_string(), rollback));
    }
    for applied in &removed {
        finish_projection(applied)?;
    }
    let _ = fs::remove_dir_all(&skill.path);
    Ok(format!("Uninstalled skill {id}\nbackup {}", backup.id))
}

pub fn list_backups(home: &Path) -> Result<String, String> {
    let records = read_backup_index(home)?;
    if records.is_empty() {
        return Ok("No skill backups.".to_string());
    }
    let mut out = String::from("Skill backups\n");
    for record in records {
        out.push_str(&format!(
            "- {} · skill {} · {} · {}\n",
            record.id,
            record.skill_id,
            record.created_at,
            &record.sha256[..12]
        ));
    }
    Ok(out)
}

pub fn restore_backup(home: &Path, backup_id: &str, dry_run: bool) -> Result<String, String> {
    let records = read_backup_index(home)?;
    let backup = records
        .iter()
        .find(|record| record.id == backup_id)
        .ok_or_else(|| format!("unknown skill backup: {backup_id}"))?
        .clone();
    let source = PathBuf::from(&backup.path);
    validate_skill_dir(&source)?;
    if dry_run {
        return Ok(format!(
            "Dry-run skill restore\nbackup {}\nskill {}\npath {}",
            backup.id, backup.skill_id, backup.path
        ));
    }
    if read_store(home)?.skills.contains_key(&backup.skill_id) {
        uninstall_skill(home, &backup.skill_id, false)?;
    }
    import_from_prepared(
        home,
        &backup.skill_id,
        &backup.skill_id,
        &source,
        SkillSyncMethod::Copy,
        backup.origin.unwrap_or(SkillOrigin::Local {
            source: backup.path.clone(),
        }),
        false,
    )
}

pub fn set_enabled(
    home: &Path,
    id: &str,
    target: AgentTarget,
    enabled: bool,
    dry_run: bool,
) -> Result<String, String> {
    let mut store = read_store(home)?;
    let skill = store
        .skills
        .get_mut(id)
        .ok_or_else(|| format!("unknown skill: {id}"))?;
    let source = PathBuf::from(&skill.path);
    let sync_method = skill.sync_method;
    validate_skill_dir(&source)?;
    let destination = app_root(home, target)?.join(id);
    if dry_run {
        return Ok(format!(
            "Dry-run skill {}\n{} -> {}\nmethod {:?}",
            if enabled { "enable" } else { "disable" },
            source.display(),
            destination.display(),
            sync_method
        ));
    }
    if enabled {
        create_projection(&source, &destination, sync_method, id)?;
    } else {
        remove_projection(&source, &destination, id)?;
    }
    skill.targets.set(target, enabled);
    let patch = store_patch(home, &store)?;
    if let Err(error) = apply_patch(
        &patch,
        PatchOptions {
            dry_run: false,
            backup: true,
        },
    ) {
        if enabled {
            let _ = remove_projection(&source, &destination, id);
        } else {
            let _ = create_projection(&source, &destination, sync_method, id);
        }
        return Err(error.to_string());
    }
    Ok(format!(
        "{} skill {id} for {}",
        if enabled { "Enabled" } else { "Disabled" },
        target.label()
    ))
}

pub fn projection_patch(
    home: &Path,
    skill: &SkillRecord,
    target: AgentTarget,
    enabled: bool,
) -> Result<ProjectionPatch, String> {
    let source = PathBuf::from(&skill.path);
    validate_skill_dir(&source)?;
    let destination = app_root(home, target)?.join(&skill.id);
    let before = inspect_projection(&source, &destination, &skill.id)?;
    Ok(ProjectionPatch {
        id: skill.id.clone(),
        source,
        destination,
        method: skill.sync_method,
        before,
        after: if enabled {
            ProjectionState::Present
        } else {
            ProjectionState::Absent
        },
    })
}

pub fn apply_projection_patch(
    patch: &ProjectionPatch,
) -> Result<Option<AppliedProjection>, String> {
    let current = inspect_projection(&patch.source, &patch.destination, &patch.id)?;
    if current != patch.before {
        return Err(format!(
            "{} changed after preview; refresh before applying",
            patch.destination.display()
        ));
    }
    if patch.before == patch.after {
        return Ok(None);
    }
    match patch.after {
        ProjectionState::Present => {
            create_projection(&patch.source, &patch.destination, patch.method, &patch.id)?;
            Ok(Some(AppliedProjection::Created(patch.clone())))
        }
        ProjectionState::Absent => {
            let quarantine = quarantine_path(&patch.destination);
            fs::rename(&patch.destination, &quarantine).map_err(|error| {
                format!(
                    "quarantine {} as {}: {error}",
                    patch.destination.display(),
                    quarantine.display()
                )
            })?;
            Ok(Some(AppliedProjection::Quarantined {
                patch: patch.clone(),
                quarantine,
            }))
        }
    }
}

pub fn rollback_projection(applied: &AppliedProjection) -> Result<(), String> {
    match applied {
        AppliedProjection::Created(patch) => {
            remove_projection(&patch.source, &patch.destination, &patch.id)
        }
        AppliedProjection::Quarantined { patch, quarantine } => {
            if patch.destination.symlink_metadata().is_ok() {
                return Err(format!(
                    "cannot restore Skill projection over existing path: {}",
                    patch.destination.display()
                ));
            }
            fs::rename(quarantine, &patch.destination).map_err(|error| {
                format!(
                    "restore {} from quarantine: {error}",
                    patch.destination.display()
                )
            })
        }
    }
}

pub fn finish_projection(applied: &AppliedProjection) -> Result<(), String> {
    let AppliedProjection::Quarantined { quarantine, .. } = applied else {
        return Ok(());
    };
    let metadata = match quarantine.symlink_metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("inspect {}: {error}", quarantine.display())),
    };
    if metadata.file_type().is_symlink() {
        fs::remove_file(quarantine)
    } else {
        fs::remove_dir_all(quarantine)
    }
    .map_err(|error| format!("remove quarantine {}: {error}", quarantine.display()))
}

fn inspect_projection(
    source: &Path,
    destination: &Path,
    id: &str,
) -> Result<ProjectionState, String> {
    let metadata = match destination.symlink_metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ProjectionState::Absent)
        }
        Err(error) => return Err(format!("inspect {}: {error}", destination.display())),
    };
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(destination)
            .map_err(|error| format!("read link {}: {error}", destination.display()))?;
        if target == source {
            Ok(ProjectionState::Present)
        } else {
            Err(format!(
                "unmanaged Skill symlink blocks projection: {}",
                destination.display()
            ))
        }
    } else if metadata.is_dir() {
        let marker = fs::read_to_string(destination.join(".xu-skill-owner")).map_err(|_| {
            format!(
                "unmanaged Skill directory blocks projection: {}",
                destination.display()
            )
        })?;
        if marker == id {
            Ok(ProjectionState::Present)
        } else {
            Err(format!(
                "Skill ownership marker mismatch: {}",
                destination.display()
            ))
        }
    } else {
        Err(format!(
            "unmanaged path blocks Skill projection: {}",
            destination.display()
        ))
    }
}

fn quarantine_path(destination: &Path) -> PathBuf {
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("skill");
    let mut counter = 0_u32;
    loop {
        let path = parent.join(format!(
            ".{name}.xu-rollback-{}-{counter}",
            std::process::id()
        ));
        if path.symlink_metadata().is_err() {
            return path;
        }
        counter += 1;
    }
}

pub fn hash_directory(path: &Path) -> Result<String, String> {
    let mut files = Vec::new();
    collect_files(path, path, &mut files)?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hasher = Sha256::new();
    for (relative, file) in files {
        hasher.update(relative.as_bytes());
        hasher.update([0]);
        let mut input =
            fs::File::open(&file).map_err(|error| format!("read {}: {error}", file.display()))?;
        let mut buffer = [0_u8; 8192];
        loop {
            let read = input
                .read(&mut buffer)
                .map_err(|error| format!("read {}: {error}", file.display()))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        hasher.update([0xff]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || !id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
    {
        Err("skill id must use ASCII letters, digits, '-' or '_'".to_string())
    } else {
        Ok(())
    }
}

fn staging_root(home: &Path) -> PathBuf {
    home.join(".codex/xu-skill-staging")
}

fn import_from_prepared(
    home: &Path,
    id: &str,
    name: &str,
    source: &Path,
    method: SkillSyncMethod,
    origin: SkillOrigin,
    dry_run: bool,
) -> Result<String, String> {
    validate_id(id)?;
    validate_skill_dir(source)?;
    let hash = hash_directory(source)?;
    let mut store = read_store(home)?;
    if store.skills.contains_key(id) {
        return Err(format!("skill already exists: {id}"));
    }
    let destination = source_root(home).join(id);
    if destination.exists() {
        return Err(format!(
            "skill destination already exists: {}",
            destination.display()
        ));
    }
    let record = SkillRecord {
        id: id.to_string(),
        name: name.to_string(),
        path: destination.display().to_string(),
        sha256: hash,
        sync_method: method,
        targets: SkillTargets::default(),
        description: skill_description(source)?,
        origin: Some(origin),
    };
    record.validate()?;
    store.skills.insert(id.to_string(), record);
    let patch = store_patch(home, &store)?;
    if dry_run {
        let result = apply_patch(
            &patch,
            PatchOptions {
                dry_run: true,
                backup: true,
            },
        )
        .map_err(|error| error.to_string())?;
        return Ok(format!(
            "Dry-run skill install\nsource {}\ndestination {}\n{}",
            source.display(),
            destination.display(),
            result.diff
        ));
    }
    let temp = prepare_directory_copy(source, &destination)?;
    fs::rename(&temp, &destination).map_err(|error| format!("install skill: {error}"))?;
    if let Err(error) = apply_patch(
        &patch,
        PatchOptions {
            dry_run: false,
            backup: true,
        },
    ) {
        let _ = fs::remove_dir_all(&destination);
        return Err(error.to_string());
    }
    Ok(format!("Installed skill {id}\n{}", destination.display()))
}

fn extract_zip(zip_path: &Path, destination: &Path) -> Result<(), String> {
    fs::create_dir_all(destination)
        .map_err(|error| format!("create {}: {error}", destination.display()))?;
    let result = extract_zip_entries(zip_path, destination);
    if result.is_err() {
        let _ = fs::remove_dir_all(destination);
    }
    result
}

fn extract_zip_entries(zip_path: &Path, destination: &Path) -> Result<(), String> {
    let file = fs::File::open(zip_path)
        .map_err(|error| format!("open ZIP {}: {error}", zip_path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|error| format!("read ZIP {}: {error}", zip_path.display()))?;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("read ZIP entry {index}: {error}"))?;
        let relative = entry
            .enclosed_name()
            .ok_or_else(|| format!("ZIP contains unsafe path: {}", entry.name()))?;
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err(format!(
                "ZIP contains unsupported symlink: {}",
                entry.name()
            ));
        }
        let output = destination.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(&output)
                .map_err(|error| format!("create {}: {error}", output.display()))?;
            continue;
        }
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("create {}: {error}", parent.display()))?;
        }
        let mut target = fs::File::create(&output)
            .map_err(|error| format!("create {}: {error}", output.display()))?;
        std::io::copy(&mut entry, &mut target)
            .map_err(|error| format!("extract {}: {error}", output.display()))?;
        target
            .flush()
            .map_err(|error| format!("flush {}: {error}", output.display()))?;
    }
    Ok(())
}

fn rollback_refreshed_projections(source: &Path, id: &str, refreshed: &[(PathBuf, PathBuf)]) {
    for (destination, quarantine) in refreshed.iter().rev() {
        let _ = remove_projection(source, destination, id);
        let _ = fs::rename(quarantine, destination);
    }
}

fn rollback_applied_projections(applied: &[AppliedProjection]) -> Result<(), String> {
    let errors = applied
        .iter()
        .rev()
        .filter_map(|item| rollback_projection(item).err())
        .collect::<Vec<_>>();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn with_rollback_error(error: String, rollback: Result<(), String>) -> String {
    match rollback {
        Ok(()) => error,
        Err(rollback) => format!("{error}; rollback failed: {rollback}"),
    }
}

fn clone_github_repo(
    owner: &str,
    repo: &str,
    branch: &str,
    destination: &Path,
) -> Result<(), String> {
    if owner.is_empty()
        || repo.is_empty()
        || !owner
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        || !repo
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err("GitHub owner/repo must use safe ASCII identifiers".to_string());
    }
    if branch.is_empty()
        || !branch
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/'))
    {
        return Err("GitHub branch must use safe ASCII identifiers".to_string());
    }
    let url = format!("https://github.com/{owner}/{repo}.git");
    let status = std::process::Command::new("git")
        .args([
            "clone",
            "--depth",
            "1",
            "--branch",
            branch,
            &url,
            &destination.display().to_string(),
        ])
        .status()
        .map_err(|error| format!("run git clone: {error}"))?;
    if !status.success() {
        return Err(format!("git clone failed for {url}@{branch}"));
    }
    Ok(())
}

fn find_skill_root(root: &Path) -> Result<PathBuf, String> {
    if root.join("SKILL.md").is_file() {
        return Ok(root.to_path_buf());
    }
    let mut matches = Vec::new();
    collect_skill_roots(root, &mut matches)?;
    match matches.as_slice() {
        [only] => Ok(only.clone()),
        [] => Err(format!("no SKILL.md found under {}", root.display())),
        many => Err(format!(
            "multiple SKILL.md roots found; pass --subdir: {}",
            many.iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn collect_skill_roots(current: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in
        fs::read_dir(current).map_err(|error| format!("read {}: {error}", current.display()))?
    {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|error| error.to_string())?;
        if file_type.is_dir() {
            if path.join("SKILL.md").is_file() {
                out.push(path);
            } else {
                collect_skill_roots(&path, out)?;
            }
        }
    }
    Ok(())
}

fn fetch_origin_stage(home: &Path, id: &str, origin: &SkillOrigin) -> Result<PathBuf, String> {
    let stage_parent = staging_root(home).join(format!("update-{id}-{}", std::process::id()));
    if stage_parent.exists() {
        fs::remove_dir_all(&stage_parent)
            .map_err(|error| format!("remove stale update stage: {error}"))?;
    }
    fs::create_dir_all(&stage_parent)
        .map_err(|error| format!("create {}: {error}", stage_parent.display()))?;
    match origin {
        SkillOrigin::Local { source } | SkillOrigin::Zip { source } => {
            let source_path = PathBuf::from(source);
            if source_path
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case("zip"))
            {
                extract_zip(&source_path, &stage_parent)?;
                find_skill_root(&stage_parent)
            } else {
                validate_skill_dir(&source_path)?;
                let destination = stage_parent.join("skill");
                copy_directory(&source_path, &destination)?;
                Ok(destination)
            }
        }
        SkillOrigin::GitHub {
            owner,
            repo,
            branch,
            subdir,
        } => {
            let clone_root = stage_parent.join("repo");
            clone_github_repo(owner, repo, branch, &clone_root)?;
            match subdir.as_deref() {
                Some(value) if !value.is_empty() => {
                    let path = resolve_skill_subdir(&clone_root, value)?;
                    validate_skill_dir(&path)?;
                    Ok(path)
                }
                _ => find_skill_root(&clone_root),
            }
        }
    }
}

fn create_skill_backup(home: &Path, skill: &SkillRecord) -> Result<SkillBackupRecord, String> {
    let source = PathBuf::from(&skill.path);
    validate_skill_dir(&source)?;
    let root = backup_root(home);
    fs::create_dir_all(&root).map_err(|error| format!("create {}: {error}", root.display()))?;
    let created_at = chrono::Utc::now().to_rfc3339();
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
    let backup_id = format!("{}-{}", skill.id, stamp);
    let destination = root.join(&backup_id);
    let temp = prepare_directory_copy(&source, &destination)?;
    fs::rename(&temp, &destination).map_err(|error| format!("install skill backup: {error}"))?;
    let record = SkillBackupRecord {
        id: backup_id,
        skill_id: skill.id.clone(),
        created_at,
        path: destination.display().to_string(),
        sha256: hash_directory(&destination)?,
        origin: skill.origin.clone(),
    };
    let mut records = read_backup_index(home)?;
    records.retain(|item| item.id != record.id);
    records.push(record.clone());
    records.sort_by(|left, right| right.created_at.cmp(&left.created_at));
    let stale = if records.len() > 20 {
        records.split_off(20)
    } else {
        Vec::new()
    };
    if let Err(error) = write_backup_index(home, &records) {
        let _ = fs::remove_dir_all(&destination);
        return Err(error);
    }
    for record in stale {
        let _ = fs::remove_dir_all(record.path);
    }
    Ok(record)
}

fn resolve_skill_subdir(root: &Path, value: &str) -> Result<PathBuf, String> {
    let relative = Path::new(value);
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err("GitHub skill subdir must be a relative path without '..'".to_string());
    }
    Ok(root.join(relative))
}

fn validate_skill_dir(path: &Path) -> Result<(), String> {
    if path
        .symlink_metadata()
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(format!(
            "skill path cannot be a symlink: {}",
            path.display()
        ));
    }
    if !path.is_dir() {
        return Err(format!("skill path is not a directory: {}", path.display()));
    }
    if !path.join("SKILL.md").is_file() {
        return Err(format!(
            "skill is missing {}",
            path.join("SKILL.md").display()
        ));
    }
    Ok(())
}

fn collect_files(
    root: &Path,
    current: &Path,
    out: &mut Vec<(String, PathBuf)>,
) -> Result<(), String> {
    for entry in
        fs::read_dir(current).map_err(|error| format!("read {}: {error}", current.display()))?
    {
        let entry = entry.map_err(|error| error.to_string())?;
        let file_type = entry.file_type().map_err(|error| error.to_string())?;
        let path = entry.path();
        if file_type.is_symlink() {
            return Err(format!(
                "skill source cannot contain symlink: {}",
                path.display()
            ));
        }
        if file_type.is_dir() {
            collect_files(root, &path, out)?;
        } else if file_type.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|error| error.to_string())?
                .to_string_lossy()
                .replace('\\', "/");
            out.push((relative, path));
        }
    }
    Ok(())
}

fn prepare_directory_copy(source: &Path, destination: &Path) -> Result<PathBuf, String> {
    let parent = destination
        .parent()
        .ok_or_else(|| "skill destination has no parent".to_string())?;
    fs::create_dir_all(parent).map_err(|error| format!("create {}: {error}", parent.display()))?;
    let temp = parent.join(format!(
        ".{}.xu-tmp-{}",
        destination
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("skill"),
        std::process::id()
    ));
    if temp.exists() {
        fs::remove_dir_all(&temp).map_err(|error| format!("remove stale temp: {error}"))?;
    }
    copy_directory(source, &temp)?;
    validate_skill_dir(&temp)?;
    Ok(temp)
}

fn copy_directory(source: &Path, destination: &Path) -> Result<(), String> {
    fs::create_dir_all(destination)
        .map_err(|error| format!("create {}: {error}", destination.display()))?;
    for entry in
        fs::read_dir(source).map_err(|error| format!("read {}: {error}", source.display()))?
    {
        let entry = entry.map_err(|error| error.to_string())?;
        let file_type = entry.file_type().map_err(|error| error.to_string())?;
        let target = destination.join(entry.file_name());
        if file_type.is_symlink() {
            return Err(format!(
                "skill source cannot contain symlink: {}",
                entry.path().display()
            ));
        } else if file_type.is_dir() {
            copy_directory(&entry.path(), &target)?;
        } else if file_type.is_file() {
            fs::copy(entry.path(), &target)
                .map_err(|error| format!("copy {}: {error}", entry.path().display()))?;
        }
    }
    Ok(())
}

fn create_projection(
    source: &Path,
    destination: &Path,
    method: SkillSyncMethod,
    id: &str,
) -> Result<(), String> {
    if destination.exists() || destination.symlink_metadata().is_ok() {
        return Err(format!(
            "skill target already exists: {}",
            destination.display()
        ));
    }
    let parent = destination
        .parent()
        .ok_or_else(|| "skill target has no parent".to_string())?;
    fs::create_dir_all(parent).map_err(|error| format!("create {}: {error}", parent.display()))?;
    match method {
        SkillSyncMethod::Symlink => symlink(source, destination)
            .map_err(|error| format!("link {}: {error}", destination.display()))?,
        SkillSyncMethod::Copy => {
            let temp = prepare_directory_copy(source, destination)?;
            atomic_write(&temp.join(".xu-skill-owner"), id.as_bytes())
                .map_err(|error| format!("write ownership marker: {error}"))?;
            fs::rename(&temp, destination)
                .map_err(|error| format!("install {}: {error}", destination.display()))?;
        }
    }
    Ok(())
}

fn remove_projection(source: &Path, destination: &Path, id: &str) -> Result<(), String> {
    let metadata = match destination.symlink_metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("inspect {}: {error}", destination.display())),
    };
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(destination)
            .map_err(|error| format!("read link {}: {error}", destination.display()))?;
        if target != source {
            return Err(format!(
                "refusing to remove unmanaged symlink: {}",
                destination.display()
            ));
        }
        fs::remove_file(destination)
            .map_err(|error| format!("remove {}: {error}", destination.display()))?;
    } else if metadata.is_dir() {
        let marker = fs::read_to_string(destination.join(".xu-skill-owner")).map_err(|_| {
            format!(
                "refusing to remove unmanaged skill directory: {}",
                destination.display()
            )
        })?;
        if marker != id {
            return Err(format!(
                "skill ownership marker mismatch: {}",
                destination.display()
            ));
        }
        fs::remove_dir_all(destination)
            .map_err(|error| format!("remove {}: {error}", destination.display()))?;
    } else {
        return Err(format!(
            "refusing to remove unmanaged path: {}",
            destination.display()
        ));
    }
    Ok(())
}

fn skill_description(path: &Path) -> Result<Option<String>, String> {
    let content = fs::read_to_string(path.join("SKILL.md"))
        .map_err(|error| format!("read SKILL.md: {error}"))?;
    Ok(content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("---") && !line.starts_with('#'))
        .map(str::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(root: &Path) -> PathBuf {
        let path = root.join("source");
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("SKILL.md"), "# Test\n\nUseful skill").unwrap();
        fs::write(path.join("data.txt"), "hello").unwrap();
        path
    }

    #[test]
    fn hash_is_stable_and_changes_with_content() {
        let root = tempfile::tempdir().unwrap();
        let path = source(root.path());
        let first = hash_directory(&path).unwrap();
        assert_eq!(first, hash_directory(&path).unwrap());
        fs::write(path.join("data.txt"), "changed").unwrap();
        assert_ne!(first, hash_directory(&path).unwrap());
    }

    #[test]
    fn import_and_symlink_projection_are_owned_and_reversible() {
        let home = tempfile::tempdir().unwrap();
        let source = source(home.path());
        import_local(
            home.path(),
            "test",
            "Test",
            &source,
            SkillSyncMethod::Symlink,
            false,
        )
        .unwrap();
        set_enabled(home.path(), "test", AgentTarget::ClaudeCode, true, false).unwrap();
        let target = home.path().join(".claude/skills/test");
        assert!(target.symlink_metadata().unwrap().file_type().is_symlink());
        set_enabled(home.path(), "test", AgentTarget::ClaudeCode, false, false).unwrap();
        assert!(target.symlink_metadata().is_err());
    }

    #[test]
    fn refuses_to_remove_unmanaged_directory() {
        let home = tempfile::tempdir().unwrap();
        let source = source(home.path());
        import_local(
            home.path(),
            "test",
            "Test",
            &source,
            SkillSyncMethod::Copy,
            false,
        )
        .unwrap();
        let target = home.path().join(".codex/skills/test");
        fs::create_dir_all(&target).unwrap();
        assert!(set_enabled(home.path(), "test", AgentTarget::Codex, false, false).is_err());
    }

    #[test]
    fn quarantined_copy_rollback_restores_exact_projection() {
        let home = tempfile::tempdir().unwrap();
        let source = source(home.path());
        import_local(
            home.path(),
            "test",
            "Test",
            &source,
            SkillSyncMethod::Copy,
            false,
        )
        .unwrap();
        set_enabled(home.path(), "test", AgentTarget::Codex, true, false).unwrap();
        let store = read_store(home.path()).unwrap();
        let record = &store.skills["test"];
        let destination = app_root(home.path(), AgentTarget::Codex)
            .unwrap()
            .join("test");
        fs::write(destination.join("data.txt"), "local projection edit").unwrap();
        let patch = projection_patch(home.path(), record, AgentTarget::Codex, false).unwrap();
        let applied = apply_projection_patch(&patch).unwrap().unwrap();
        assert!(destination.symlink_metadata().is_err());
        rollback_projection(&applied).unwrap();
        assert_eq!(
            fs::read_to_string(destination.join("data.txt")).unwrap(),
            "local projection edit"
        );
    }

    #[test]
    fn zip_install_update_uninstall_and_restore_are_reversible() {
        let home = tempfile::tempdir().unwrap();
        let package_root = home.path().join("package");
        let nested = package_root.join("skill-root");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("SKILL.md"), "# Zip Skill\n\nFrom zip").unwrap();
        fs::write(nested.join("data.txt"), "v1").unwrap();
        let zip_path = home.path().join("skill.zip");
        let status = std::process::Command::new("zip")
            .args(["-qr"])
            .arg(&zip_path)
            .arg("skill-root")
            .current_dir(&package_root)
            .status()
            .unwrap();
        assert!(status.success());

        let dry = install_zip(
            home.path(),
            "zipskill",
            "Zip Skill",
            &zip_path,
            SkillSyncMethod::Copy,
            true,
        )
        .unwrap();
        assert!(dry.contains("Dry-run skill install"));
        assert!(!home.path().join(".codex/xu-skills/zipskill").exists());

        install_zip(
            home.path(),
            "zipskill",
            "Zip Skill",
            &zip_path,
            SkillSyncMethod::Copy,
            false,
        )
        .unwrap();
        set_enabled(home.path(), "zipskill", AgentTarget::OpenCode, true, false).unwrap();
        assert!(home
            .path()
            .join(".config/opencode/skills/zipskill")
            .exists());

        fs::write(nested.join("data.txt"), "v2").unwrap();
        let zip_path_v2 = home.path().join("skill-v2.zip");
        let status = std::process::Command::new("zip")
            .args(["-qr"])
            .arg(&zip_path_v2)
            .arg("skill-root")
            .current_dir(&package_root)
            .status()
            .unwrap();
        assert!(status.success());
        let mut store = read_store(home.path()).unwrap();
        store.skills.get_mut("zipskill").unwrap().origin = Some(SkillOrigin::Zip {
            source: zip_path_v2.display().to_string(),
        });
        apply_patch(
            &store_patch(home.path(), &store).unwrap(),
            PatchOptions {
                dry_run: false,
                backup: false,
            },
        )
        .unwrap();
        let updated = update_skill(home.path(), "zipskill", false).unwrap();
        assert!(updated.contains("Updated skill"));
        assert_eq!(
            fs::read_to_string(home.path().join(".codex/xu-skills/zipskill/data.txt")).unwrap(),
            "v2"
        );
        assert_eq!(
            fs::read_to_string(
                home.path()
                    .join(".config/opencode/skills/zipskill/data.txt")
            )
            .unwrap(),
            "v2"
        );

        let uninstalled = uninstall_skill(home.path(), "zipskill", false).unwrap();
        assert!(uninstalled.contains("backup"));
        assert!(!home.path().join(".codex/xu-skills/zipskill").exists());
        assert!(!home
            .path()
            .join(".config/opencode/skills/zipskill")
            .exists());
        let backups = read_backup_index(home.path()).unwrap();
        assert_eq!(backups.len(), 2);
        restore_backup(home.path(), &backups[0].id, false).unwrap();
        assert!(home.path().join(".codex/xu-skills/zipskill").exists());
    }

    #[test]
    fn zip_install_rejects_path_traversal_without_writing_outside_stage() {
        use zip::write::SimpleFileOptions;

        let home = tempfile::tempdir().unwrap();
        let zip_path = home.path().join("unsafe.zip");
        let file = fs::File::create(&zip_path).unwrap();
        let mut archive = zip::ZipWriter::new(file);
        archive
            .start_file("../escaped.txt", SimpleFileOptions::default())
            .unwrap();
        archive.write_all(b"escaped").unwrap();
        archive.finish().unwrap();

        let error = install_zip(
            home.path(),
            "unsafe",
            "Unsafe",
            &zip_path,
            SkillSyncMethod::Copy,
            false,
        )
        .unwrap_err();
        assert!(error.contains("unsafe path"));
        assert!(!home.path().join(".codex/escaped.txt").exists());
        assert!(!home.path().join(".codex/xu-skills/unsafe").exists());
    }

    #[test]
    fn github_subdir_cannot_escape_clone_root() {
        let root = tempfile::tempdir().unwrap();
        assert!(resolve_skill_subdir(root.path(), "skills/example").is_ok());
        assert!(resolve_skill_subdir(root.path(), "../outside").is_err());
        assert!(resolve_skill_subdir(root.path(), "/outside").is_err());
    }
}
