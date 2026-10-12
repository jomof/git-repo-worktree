use crate::util::{absolutize, normalize, path_is_safe_relative};
use serde::Serialize;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Resolution {
    pub input: String,
    pub project: String,
}

/// Resolves user-supplied project paths to manifest project paths.
///
/// An input may name a project exactly, name any path inside a project (the
/// longest enclosing project wins), or be a filesystem path (absolute, `./`,
/// or `../`) inside one of `roots`. A bare relative input that matches nothing
/// is retried relative to `cwd` when `cwd` is inside one of `roots`.
pub fn resolve_projects(
    inputs: &[String],
    known: &[String],
    roots: &[PathBuf],
    cwd: &Path,
) -> Result<Vec<Resolution>, Vec<String>> {
    let mut resolved = Vec::new();
    let mut unknown = Vec::new();
    for input in inputs {
        match resolve_one(input, known, roots, cwd) {
            Some(project) => {
                if !resolved
                    .iter()
                    .any(|existing: &Resolution| existing.project == project)
                {
                    resolved.push(Resolution {
                        input: input.clone(),
                        project,
                    });
                }
            }
            None => unknown.push(input.clone()),
        }
    }
    if unknown.is_empty() {
        Ok(resolved)
    } else {
        Err(unknown)
    }
}

/// Whether an input names a filesystem location (absolute, `./`, or `../`)
/// rather than a manifest-relative project path.
pub fn is_filesystem_style(input: &str) -> bool {
    let path = Path::new(input);
    path.is_absolute()
        || matches!(
            path.components().next(),
            Some(Component::CurDir | Component::ParentDir)
        )
}

fn resolve_one(input: &str, known: &[String], roots: &[PathBuf], cwd: &Path) -> Option<String> {
    let path = Path::new(input);
    if is_filesystem_style(input) {
        let absolute = absolutize(path, cwd);
        return roots
            .iter()
            .find_map(|root| absolute.strip_prefix(root).ok())
            .and_then(|relative| lookup(relative, known));
    }
    if !path_is_safe_relative(path) {
        return None;
    }
    lookup(&normalize(path), known).or_else(|| {
        roots.iter().find_map(|root| {
            let relative = cwd.strip_prefix(root).ok()?;
            if relative.as_os_str().is_empty() {
                return None;
            }
            lookup(&normalize(&relative.join(path)), known)
        })
    })
}

fn lookup(relative: &Path, known: &[String]) -> Option<String> {
    if relative.as_os_str().is_empty() {
        return None;
    }
    known
        .iter()
        .filter(|project| relative.starts_with(Path::new(project.as_str())))
        .max_by_key(|project| Path::new(project.as_str()).components().count())
        .cloned()
}

/// Human-readable notes for inputs that resolved to a different project path.
pub fn rewritten(resolutions: &[Resolution]) -> Vec<&Resolution> {
    resolutions
        .iter()
        .filter(|resolution| {
            normalize(Path::new(&resolution.input)) != Path::new(&resolution.project)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known() -> Vec<String> {
        [
            "tools/adt/idea",
            "tools/vendor/google",
            "tools/vendor/google/x",
            "prebuilts/tools",
        ]
        .iter()
        .map(|path| path.to_string())
        .collect()
    }

    fn one(input: &str, cwd: &str) -> Option<String> {
        resolve_projects(
            &[input.to_string()],
            &known(),
            &[PathBuf::from("/tree"), PathBuf::from("/src")],
            Path::new(cwd),
        )
        .ok()
        .map(|resolved| resolved[0].project.clone())
    }

    #[test]
    fn exact_and_subpath_inputs_resolve_to_the_longest_enclosing_project() {
        assert_eq!(
            one("tools/adt/idea", "/elsewhere").as_deref(),
            Some("tools/adt/idea")
        );
        assert_eq!(
            one("tools/adt/idea/", "/elsewhere").as_deref(),
            Some("tools/adt/idea")
        );
        assert_eq!(
            one("tools/vendor/google/android-apk", "/elsewhere").as_deref(),
            Some("tools/vendor/google")
        );
        assert_eq!(
            one("tools/vendor/google/x/deep", "/elsewhere").as_deref(),
            Some("tools/vendor/google/x")
        );
        assert_eq!(
            one("prebuilts/tools/common/m2", "/elsewhere").as_deref(),
            Some("prebuilts/tools")
        );
        assert_eq!(one("tools/vendor", "/elsewhere"), None);
        assert_eq!(one("../escape", "/elsewhere"), None);
    }

    #[test]
    fn filesystem_and_cwd_relative_inputs_resolve_inside_roots() {
        assert_eq!(
            one("/tree/tools/adt/idea/android/src", "/").as_deref(),
            Some("tools/adt/idea")
        );
        assert_eq!(
            one("./android", "/tree/tools/adt/idea").as_deref(),
            Some("tools/adt/idea")
        );
        assert_eq!(
            one("android", "/src/tools/adt/idea").as_deref(),
            Some("tools/adt/idea")
        );
        assert_eq!(
            one(".", "/tree/prebuilts/tools/common").as_deref(),
            Some("prebuilts/tools")
        );
        assert_eq!(one("/outside/tools/adt/idea", "/"), None);
    }

    #[test]
    fn reports_unknown_inputs_and_dedupes_projects() {
        let result = resolve_projects(
            &["tools/adt/idea/a".into(), "tools/adt/idea/b".into()],
            &known(),
            &[],
            Path::new("/"),
        )
        .unwrap();
        assert_eq!(result.len(), 1);
        let error = resolve_projects(&["nope".into()], &known(), &[], Path::new("/")).unwrap_err();
        assert_eq!(error, vec!["nope".to_string()]);
    }
}
