use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::domain::AgentTarget;

pub const FINGERPRINT_FILE: &str = ".sync-skill.fp";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    pub unchanged: usize,
    pub details: Vec<String>,
}

pub fn skills_dir(home: &Path, target: AgentTarget) -> Result<PathBuf, String> {
    match target {
        AgentTarget::OpenCode => Ok(home.join(".config/opencode/skills")),
        AgentTarget::ClaudeCode => Ok(home.join(".claude/skills")),
        AgentTarget::Codex => Ok(home.join(".codex/skills")),
        AgentTarget::Other => Err("unsupported agent target: other".to_string()),
    }
}

pub fn sync_skills(
    home: &Path,
    source: AgentTarget,
    targets: &[AgentTarget],
    dry_run: bool,
) -> Result<SyncReport, String> {
    if targets.is_empty() {
        return Err("no sync targets given".to_string());
    }
    let src_dir = skills_dir(home, source)?;
    if !src_dir.is_dir() {
        return Err(format!(
            "source skills directory not found: {}",
            src_dir.display()
        ));
    }
    let mut seen = BTreeSet::new();
    let mut target_list: Vec<AgentTarget> = Vec::new();
    for target in targets {
        if *target == source {
            continue;
        }
        if seen.insert(*target) {
            target_list.push(*target);
        }
    }
    let skills = list_skills(&src_dir)?;
    let mut report = SyncReport::default();
    for target in target_list {
        let dest_dir = skills_dir(home, target)?;
        sync_to_target(&src_dir, &dest_dir, &skills, dry_run, &mut report)?;
    }
    Ok(report)
}

fn list_skills(src_dir: &Path) -> Result<BTreeSet<String>, String> {
    let mut out = BTreeSet::new();
    let entries = fs::read_dir(src_dir).map_err(|e| format!("read {}: {e}", src_dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if !entry.path().is_dir() {
            continue;
        }
        if !entry.path().join("SKILL.md").is_file() {
            continue;
        }
        out.insert(name);
    }
    Ok(out)
}

fn sync_to_target(
    src_dir: &Path,
    dest_dir: &Path,
    src_skills: &BTreeSet<String>,
    dry_run: bool,
    report: &mut SyncReport,
) -> Result<(), String> {
    report.details.push(format!("== {} ==", dest_dir.display()));
    for name in src_skills {
        let src_path = src_dir.join(name);
        let fp = fingerprint(&src_path)?;
        let target_path = dest_dir.join(name);
        let fp_path = target_path.join(FINGERPRINT_FILE);
        let stored = fs::read_to_string(&fp_path)
            .ok()
            .map(|s| s.trim().to_string());
        if stored.as_deref() == Some(fp.as_str()) {
            report.unchanged += 1;
            continue;
        }
        let existed = target_path.is_dir();
        if dry_run {
            if existed {
                report.updated += 1;
                report.details.push(format!("[更新] {name}"));
            } else {
                report.added += 1;
                report.details.push(format!("[新增] {name}"));
            }
            continue;
        }
        if existed {
            fs::remove_dir_all(&target_path)
                .map_err(|e| format!("remove {}: {e}", target_path.display()))?;
        }
        fs::create_dir_all(dest_dir).map_err(|e| format!("mkdir {}: {e}", dest_dir.display()))?;
        copy_dir(&src_path, &target_path)?;
        fs::write(&fp_path, format!("{fp}\n"))
            .map_err(|e| format!("write {}: {e}", fp_path.display()))?;
        if existed {
            report.updated += 1;
            report.details.push(format!("[更新] {name}"));
        } else {
            report.added += 1;
            report.details.push(format!("[新增] {name}"));
        }
    }
    if dest_dir.is_dir() {
        let entries =
            fs::read_dir(dest_dir).map_err(|e| format!("read {}: {e}", dest_dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            if src_skills.contains(&name) {
                continue;
            }
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            if !dir.join(FINGERPRINT_FILE).is_file() {
                continue;
            }
            report.removed += 1;
            report.details.push(format!("[删除] {name}"));
            if !dry_run {
                fs::remove_dir_all(&dir).map_err(|e| format!("remove {}: {e}", dir.display()))?;
            }
        }
    }
    Ok(())
}

const CONTENT_HASH_LIMIT: u64 = 256 * 1024;

fn fingerprint(dir: &Path) -> Result<String, String> {
    let mut files: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    collect_files(dir, dir, &mut files)?;
    let mut hasher = Sha256::new();
    for (rel, (len, mtime)) in &files {
        hasher.update(rel.as_bytes());
        hasher.update([0]);
        hasher.update(len.to_le_bytes());
        hasher.update([0]);
        hasher.update(mtime.to_le_bytes());
        hasher.update([0]);
        // 内容哈希：仅小文件（技能目录典型体量），防 touch 误重建与同秒等长漏检。
        if *len <= CONTENT_HASH_LIMIT {
            let bytes = fs::read(dir.join(rel))
                .map_err(|e| format!("read {}: {e}", dir.join(rel).display()))?;
            hasher.update(bytes.len().to_le_bytes());
            hasher.update([0]);
            hasher.update(Sha256::digest(&bytes));
            hasher.update([0]);
        }
    }
    Ok(hex(&hasher.finalize()))
}

fn collect_files(
    root: &Path,
    dir: &Path,
    out: &mut BTreeMap<String, (u64, u64)>,
) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("read {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == ".git" {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .into_owned();
        if entry.file_type().map_err(|e| e.to_string())?.is_symlink() {
            continue; // 不跟随符号链接：防循环递归与把外部文件纳入指纹。
        }
        if path.is_dir() {
            collect_files(root, &path, out)?;
        } else if path.is_file() {
            let md = fs::metadata(&path).map_err(|e| format!("stat {}: {e}", path.display()))?;
            let mtime = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            out.insert(rel, (md.len(), mtime));
        }
    }
    Ok(())
}

fn copy_dir(src: &Path, dest: &Path) -> Result<(), String> {
    fs::create_dir_all(dest).map_err(|e| format!("mkdir {}: {e}", dest.display()))?;
    let entries = fs::read_dir(src).map_err(|e| format!("read {}: {e}", src.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let from = entry.path();
        let to = dest.join(&name);
        if entry.file_type().map_err(|e| e.to_string())?.is_symlink() {
            continue; // 同上：镜像不搬运符号链接。
        }
        if from.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            fs::copy(&from, &to)
                .map_err(|e| format!("copy {} -> {}: {e}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn setup_home() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().to_path_buf();
        let _ = fs::create_dir_all(home.join(".config/opencode/skills"));
        let _ = fs::create_dir_all(home.join(".claude/skills"));
        let _ = fs::create_dir_all(home.join(".codex/skills"));
        (dir, home)
    }

    fn write(path: &Path, bytes: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn files_of(root: &Path) -> BTreeMap<String, Vec<u8>> {
        let mut out = BTreeMap::new();
        walk(root, root, &mut out);
        out
    }

    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                out.insert(rel, fs::read(&path).unwrap());
            }
        }
    }

    fn without_fp(tree: &BTreeMap<String, Vec<u8>>) -> BTreeMap<String, Vec<u8>> {
        tree.iter()
            .filter(|(rel, _)| !rel.ends_with(FINGERPRINT_FILE))
            .map(|(rel, bytes)| (rel.clone(), bytes.clone()))
            .collect()
    }

    #[test]
    fn mirrors_opencode_to_claude_and_codex() {
        let (dir, home) = setup_home();
        let src = home.join(".config/opencode/skills");
        write(&src.join("skill-a/SKILL.md"), "a\n");
        write(&src.join("skill-a/assets/run.sh"), "#!/bin/sh\n");
        write(&src.join("skill-b/SKILL.md"), "b\n");

        let report = sync_skills(
            &home,
            AgentTarget::OpenCode,
            &[AgentTarget::ClaudeCode, AgentTarget::Codex],
            false,
        )
        .unwrap();

        assert_eq!(report.added, 4);
        assert_eq!(report.updated, 0);
        assert_eq!(report.removed, 0);
        assert_eq!(report.unchanged, 0);

        let expected = without_fp(&files_of(&src));
        for target in [AgentTarget::ClaudeCode, AgentTarget::Codex] {
            let dest = skills_dir(&home, target).unwrap();
            let actual = without_fp(&files_of(&dest));
            assert_eq!(actual, expected, "{} content mismatch", dest.display());
            for skill in ["skill-a", "skill-b"] {
                assert!(dest.join(skill).join(FINGERPRINT_FILE).is_file());
            }
        }
        drop(dir);
    }

    #[test]
    fn second_run_is_idempotent() {
        let (dir, home) = setup_home();
        let src = home.join(".config/opencode/skills");
        write(&src.join("skill-a/SKILL.md"), "a\n");
        write(&src.join("skill-b/SKILL.md"), "b\n");

        sync_skills(
            &home,
            AgentTarget::OpenCode,
            &[AgentTarget::ClaudeCode, AgentTarget::Codex],
            false,
        )
        .unwrap();
        let before = files_of(&home.join(".claude/skills"));

        let report = sync_skills(
            &home,
            AgentTarget::OpenCode,
            &[AgentTarget::ClaudeCode, AgentTarget::Codex],
            false,
        )
        .unwrap();

        assert_eq!(report.added, 0);
        assert_eq!(report.updated, 0);
        assert_eq!(report.removed, 0);
        assert_eq!(report.unchanged, 4);
        assert_eq!(files_of(&home.join(".claude/skills")), before);
        drop(dir);
    }

    #[test]
    fn removes_fingerprinted_skills_but_keeps_foreign_ones() {
        let (dir, home) = setup_home();
        let src = home.join(".config/opencode/skills");
        write(&src.join("skill-a/SKILL.md"), "a\n");
        write(&src.join("skill-b/SKILL.md"), "b\n");
        sync_skills(
            &home,
            AgentTarget::OpenCode,
            &[AgentTarget::ClaudeCode],
            false,
        )
        .unwrap();

        fs::remove_dir_all(src.join("skill-b")).unwrap();
        let manual = home.join(".claude/skills/manual");
        write(&manual.join("SKILL.md"), "manual\n");

        let report = sync_skills(
            &home,
            AgentTarget::OpenCode,
            &[AgentTarget::ClaudeCode],
            false,
        )
        .unwrap();

        assert_eq!(report.removed, 1);
        assert_eq!(report.unchanged, 1);
        assert!(!home.join(".claude/skills/skill-b").exists());
        assert!(home.join(".claude/skills/manual/SKILL.md").is_file());
        drop(dir);
    }

    #[test]
    fn dry_run_reports_without_writing() {
        let (dir, home) = setup_home();
        let src = home.join(".config/opencode/skills");
        write(&src.join("skill-a/SKILL.md"), "a\n");
        write(&src.join("skill-b/SKILL.md"), "b\n");
        sync_skills(
            &home,
            AgentTarget::OpenCode,
            &[AgentTarget::ClaudeCode],
            false,
        )
        .unwrap();
        let before = files_of(&home.join(".claude/skills"));

        write(&src.join("skill-a/SKILL.md"), "a-updated\n");
        let report = sync_skills(
            &home,
            AgentTarget::OpenCode,
            &[AgentTarget::ClaudeCode],
            true,
        )
        .unwrap();

        assert_eq!(report.updated, 1);
        assert_eq!(report.unchanged, 1);
        assert_eq!(files_of(&home.join(".claude/skills")), before);

        let report = sync_skills(
            &home,
            AgentTarget::OpenCode,
            &[AgentTarget::ClaudeCode],
            false,
        )
        .unwrap();
        assert_eq!(report.updated, 1);
        assert_eq!(
            fs::read_to_string(home.join(".claude/skills/skill-a/SKILL.md")).unwrap(),
            "a-updated\n"
        );
        drop(dir);
    }

    #[test]
    fn dry_run_removal_keeps_files() {
        let (dir, home) = setup_home();
        let src = home.join(".config/opencode/skills");
        write(&src.join("skill-a/SKILL.md"), "a\n");
        sync_skills(
            &home,
            AgentTarget::OpenCode,
            &[AgentTarget::ClaudeCode],
            false,
        )
        .unwrap();
        fs::remove_dir_all(src.join("skill-a")).unwrap();

        let report = sync_skills(
            &home,
            AgentTarget::OpenCode,
            &[AgentTarget::ClaudeCode],
            true,
        )
        .unwrap();

        assert_eq!(report.removed, 1);
        assert!(home.join(".claude/skills/skill-a/SKILL.md").is_file());
        drop(dir);
    }

    #[test]
    fn skips_hidden_and_invalid_skills() {
        let (dir, home) = setup_home();
        let src = home.join(".config/opencode/skills");
        write(&src.join(".system/SKILL.md"), "hidden\n");
        write(&src.join("no-skill/README.md"), "no SKILL.md\n");
        write(&src.join("ok/SKILL.md"), "ok\n");

        sync_skills(
            &home,
            AgentTarget::OpenCode,
            &[AgentTarget::ClaudeCode],
            false,
        )
        .unwrap();

        let dest = home.join(".claude/skills");
        assert!(!dest.join(".system").exists());
        assert!(!dest.join("no-skill").exists());
        assert!(dest.join("ok/SKILL.md").is_file());
        drop(dir);
    }

    #[test]
    fn any_source_any_target() {
        let (dir, home) = setup_home();
        let src = home.join(".claude/skills");
        write(&src.join("skill-x/SKILL.md"), "x\n");
        write(&src.join("skill-x/references/notes.md"), "notes\n");

        let report = sync_skills(
            &home,
            AgentTarget::ClaudeCode,
            &[AgentTarget::OpenCode],
            false,
        )
        .unwrap();

        assert_eq!(report.added, 1);
        let dest = home.join(".config/opencode/skills/skill-x");
        assert_eq!(fs::read_to_string(dest.join("SKILL.md")).unwrap(), "x\n");
        assert_eq!(
            fs::read_to_string(dest.join("references/notes.md")).unwrap(),
            "notes\n"
        );
        assert!(dest.join(FINGERPRINT_FILE).is_file());
        drop(dir);
    }

    #[test]
    fn same_source_target_is_skipped() {
        let (dir, home) = setup_home();
        let src = home.join(".config/opencode/skills");
        write(&src.join("skill-a/SKILL.md"), "a\n");

        let report = sync_skills(
            &home,
            AgentTarget::OpenCode,
            &[AgentTarget::OpenCode],
            false,
        )
        .unwrap();

        assert_eq!(report.added, 0);
        assert_eq!(report.unchanged, 0);
        assert_eq!(
            fs::read_to_string(src.join("skill-a/SKILL.md")).unwrap(),
            "a\n"
        );
        drop(dir);
    }

    #[test]
    fn empty_targets_is_an_error() {
        let (dir, home) = setup_home();
        assert!(sync_skills(&home, AgentTarget::OpenCode, &[], false).is_err());
        drop(dir);
    }
}
