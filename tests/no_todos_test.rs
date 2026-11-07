use std::fs;
use std::path::Path;

#[test]
fn test_zero_todo_or_fixme_in_critical_paths() {
    let mut violations = Vec::new();
    scan_dir(Path::new("src"), &mut violations);

    if !violations.is_empty() {
        panic!(
            "Zero TODO/FIXME enforcement failed! Found {} violation(s):\n{}",
            violations.len(),
            violations.join("\n")
        );
    }
}

fn scan_dir(dir: &Path, violations: &mut Vec<String>) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                scan_dir(&path, violations);
            } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
                let content = fs::read_to_string(&path).unwrap_or_default();
                for (line_idx, line) in content.lines().enumerate() {
                    let upper = line.to_uppercase();
                    if upper.contains("TODO") || upper.contains("FIXME") {
                        violations.push(format!(
                            "{}:{}: {}",
                            path.display(),
                            line_idx + 1,
                            line.trim()
                        ));
                    }
                }
            }
        }
    }
}
