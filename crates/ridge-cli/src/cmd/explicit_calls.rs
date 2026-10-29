//! Opt-in, typed migration of zero-parameter message invocations.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use ridge_driver::{check_workspace_typed, migrate_message_calls, CheckOptions, SourceInput};
use ridge_manifest::WorkspaceRoot;

use super::fmt::FmtArgs;
use crate::{render::report_diagnostics, CliError};

struct Plan {
    path: PathBuf,
    before: String,
    after: String,
    calls: usize,
}

type Contexts = BTreeMap<PathBuf, (CheckOptions, BTreeSet<PathBuf>)>;

pub(super) fn execute(args: &FmtArgs, cwd: &Path) -> Result<(), CliError> {
    let paths = if args.paths.is_empty() {
        vec![super::workspace_root_for(cwd)?]
    } else {
        args.paths.iter().map(|path| cwd.join(path)).collect()
    };
    let files = super::fmt::expand_paths(&paths)?;
    let mut contexts = Contexts::new();
    for file in files {
        let file = ridge_driver::validate_source_file(&file).map_err(|error| {
            eprintln!("error: {error}");
            CliError::AlreadyReported
        })?;
        add_context(&mut contexts, file)?;
    }
    let mut plans = Vec::new();
    for (_, (options, selected)) in contexts {
        plans.extend(plan_context(options, &selected)?);
    }
    // Plan every selected context before making any file changes.
    for plan in &plans {
        if std::fs::read_to_string(&plan.path).map_err(|e| io_error(&plan.path, e))? != plan.before
        {
            return Err(io_error(
                &plan.path,
                "source changed during migration; run it again",
            ));
        }
    }
    for plan in &plans {
        println!(
            "{}",
            ridge_diagnostics::terminal::terminal_text(format_args!(
                "{} {} ({} call(s))",
                if args.check {
                    "would migrate"
                } else {
                    "migrate"
                },
                plan.path.display(),
                plan.calls,
            ))
        );
        if !args.check {
            write_plan(plan)?;
        }
    }
    if args.check && !plans.is_empty() {
        return Err(CliError::FmtCheckFailed { count: plans.len() });
    }
    Ok(())
}

fn add_context(contexts: &mut Contexts, file: PathBuf) -> Result<(), CliError> {
    let parent = file.parent().unwrap_or(&file);
    let (key, options) = match ridge_manifest::find_workspace_root(parent) {
        WorkspaceRoot::Found(root) => (root.clone(), CheckOptions::new(root)),
        WorkspaceRoot::NotFound => {
            let mut options = CheckOptions::new(parent.to_owned());
            options.source = SourceInput::File {
                path: file.clone(),
                require_main: false,
            };
            (file.clone(), options)
        }
        WorkspaceRoot::Malformed(_) => {
            super::workspace_root_for(parent)?;
            return Err(CliError::AlreadyReported);
        }
    };
    contexts
        .entry(key)
        .or_insert_with(|| (options, BTreeSet::new()))
        .1
        .insert(file);
    Ok(())
}

fn plan_context(
    options: CheckOptions,
    selected: &BTreeSet<PathBuf>,
) -> Result<Vec<Plan>, CliError> {
    let checked = check_workspace_typed(options).map_err(|error| {
        eprintln!("error: {error}");
        CliError::AlreadyReported
    })?;
    let other: Vec<_> = checked
        .diagnostics
        .iter()
        .filter(|d| d.code != "T063")
        .cloned()
        .collect();
    if report_diagnostics(&other, &checked.sources, false).fatal() {
        eprintln!("Resolve the reported errors before migrating calls; no files were changed.");
        return Err(CliError::AlreadyReported);
    }
    let mut found = BTreeSet::new();
    let mut plans = Vec::new();
    for module in &checked.typed.modules {
        let metadata = &checked.resolved.graph.modules[module.id.0 as usize];
        let path = ridge_manifest::canonicalize(&metadata.file_path)
            .map_err(|error| io_error(&metadata.file_path, error))?;
        if !selected.contains(&path) {
            continue;
        }
        found.insert(path.clone());
        let id = checked.sources.id_for_module(module.id);
        let before = checked
            .sources
            .text(id.as_str())
            .ok_or_else(|| io_error(&path, "checked source is unavailable"))?;
        let unresolved = module
            .message_call_sites
            .iter()
            .filter(|site| site.argument_count == 0 && site.parameter_count.is_none())
            .count();
        if unresolved != 0 {
            return Err(io_error(
                &path,
                format!(
                    "{unresolved} bare message(s) could not be resolved; annotate the receiver's actor handle type before migrating; no files were changed"
                ),
            ));
        }
        let after =
            migrate_message_calls(before, module).map_err(|error| io_error(&path, error))?;
        if before != after {
            let calls = module
                .message_call_sites
                .iter()
                .filter(|site| site.replacement().is_some())
                .count();
            plans.push(Plan {
                path,
                before: before.to_owned(),
                after,
                calls,
            });
        }
    }
    if &found != selected {
        return Err(CliError::FmtIoError {
            path: selected
                .difference(&found)
                .next()
                .cloned()
                .unwrap_or_default(),
            source: "source is outside the checked workspace; no files were changed".to_owned(),
        });
    }
    Ok(plans)
}

fn io_error(path: &Path, error: impl std::fmt::Display) -> CliError {
    CliError::FmtIoError {
        path: path.to_owned(),
        source: error.to_string(),
    }
}

fn write_plan(plan: &Plan) -> Result<(), CliError> {
    let parent = plan
        .path
        .parent()
        .ok_or_else(|| io_error(&plan.path, "source has no parent directory"))?;
    let permissions = std::fs::metadata(&plan.path)
        .map_err(|e| io_error(&plan.path, e))?
        .permissions();
    if permissions.readonly() {
        return Err(io_error(&plan.path, "source is read-only"));
    }
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|e| io_error(&plan.path, e))?;
    temporary
        .write_all(plan.after.as_bytes())
        .map_err(|e| io_error(&plan.path, e))?;
    temporary
        .as_file()
        .set_permissions(permissions)
        .map_err(|e| io_error(&plan.path, e))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|e| io_error(&plan.path, e))?;
    if std::fs::read_to_string(&plan.path).map_err(|e| io_error(&plan.path, e))? != plan.before {
        return Err(io_error(
            &plan.path,
            "source changed during migration; run it again",
        ));
    }
    temporary
        .persist(&plan.path)
        .map_err(|e| io_error(&plan.path, e.error))?;
    Ok(())
}
