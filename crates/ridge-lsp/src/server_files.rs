//! File-owned workspace contexts, including clients that opened no folder.
use super::{Workspace, WorkspaceSnapshot};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tower_lsp::lsp_types::Url;

/// Expand buffer overlays to cached source identities and return their aliases.
pub(super) fn aliases(snap: &WorkspaceSnapshot, edits: &mut Vec<(Url, String)>) -> Vec<(Url, Url)> {
    let aliases: Vec<_> = snap
        .document_contexts
        .iter()
        .filter(|(uri, context)| super::uri_key(uri) != super::uri_key(&context.canonical_uri))
        .map(|(uri, context)| (uri.clone(), context.canonical_uri.clone()))
        .collect();
    let additional: Vec<_> = aliases
        .iter()
        .filter_map(|(alias, canonical)| {
            edits
                .iter()
                .find(|(uri, _)| uri == alias)
                .map(|(_, text)| (canonical.clone(), text.clone()))
        })
        .collect();
    edits.extend(additional);
    aliases
}

/// Cached identity and policy owner of one open document.
#[derive(Debug, Clone)]
pub(super) struct DocumentContext {
    /// Canonical workspace identity, or no manifest for a genuinely loose file.
    pub root: Option<PathBuf>,
    /// Resolve source aliases once, rather than during position queries.
    pub canonical_uri: Url,
}

impl DocumentContext {
    /// Resolve policy ownership before entering the shared state lock.
    pub fn discover(uri: &Url) -> Self {
        let Some(path) = uri
            .to_file_path()
            .ok()
            .filter(|path| path.extension().is_some_and(|ext| ext == "ridge"))
        else {
            return Self {
                root: None,
                canonical_uri: uri.clone(),
            };
        };
        let path = ridge_manifest::canonicalize(&path).unwrap_or(path);
        let root = match ridge_manifest::find_workspace_root(&path) {
            ridge_manifest::WorkspaceRoot::Found(root) => Some(root),
            ridge_manifest::WorkspaceRoot::Malformed(manifest) => {
                manifest.parent().map(std::path::Path::to_owned)
            }
            ridge_manifest::WorkspaceRoot::NotFound => None,
        }
        .map(|root| ridge_manifest::canonicalize(&root).unwrap_or(root));
        Self {
            root,
            canonical_uri: Url::from_file_path(path).unwrap_or_else(|()| uri.clone()),
        }
    }
}

/// Preserve slot numbers while activating just the configured or open contexts.
/// Return the old analysis URIs whose diagnostics must be cleared.
pub(super) fn refresh(snap: &mut WorkspaceSnapshot) -> Vec<Url> {
    let roots: HashSet<PathBuf> = snap
        .document_contexts
        .values()
        .filter_map(|context| context.root.clone())
        .collect();
    let loose = snap
        .document_contexts
        .values()
        .any(|context| context.root.is_none())
        || (roots.is_empty() && !snap.workspaces.iter().any(|ws| ws.configured));
    let mut ordered: Vec<_> = roots.iter().cloned().collect();
    ordered.sort();
    for root in ordered {
        if !snap
            .workspaces
            .iter()
            .any(|ws| ws.key.as_ref() == Some(&root))
        {
            let mut ws = Workspace::new(Some(root));
            ws.configured = false;
            ws.active = false;
            snap.workspaces.push(ws);
        }
    }
    if loose && !snap.workspaces.iter().any(|ws| ws.root.is_none()) {
        snap.workspaces.push(Workspace::new(None));
    }
    let mut cleared = Vec::new();
    for ws in &mut snap.workspaces {
        let active = ws.configured || ws.key.as_ref().map_or(loose, |root| roots.contains(root));
        if ws.active && !active {
            cleared.extend(reset(ws));
        }
        ws.active = active;
    }
    cleared
}

/// Release an inferred analysis and invalidate any in-flight result for it.
fn reset(ws: &mut Workspace) -> Vec<Url> {
    let cleared = ws.index.take().map_or_else(Vec::new, |index| {
        index.uri_to_module.keys().cloned().collect()
    });
    ws.engine = Arc::new(super::StdMutex::new(None));
    ws.pending_batch = None;
    cleared
}

/// Closing a buffer reseeds only its inferred context; configured roots retain
/// their existing close behavior. Other workspaces keep their engines and index.
pub(super) fn close(snap: &mut WorkspaceSnapshot, uri: &Url) -> (Vec<Url>, HashSet<usize>) {
    let context = snap.document_contexts.remove(uri);
    let mut cleared = refresh(snap);
    let mut slots = HashSet::new();
    if let Some(context) = context {
        let owner = context.root;
        for (slot, ws) in snap.workspaces.iter_mut().enumerate() {
            if ws.active && !ws.configured && ws.key == owner {
                cleared.extend(reset(ws));
                slots.insert(slot);
            }
        }
    }
    (cleared, slots)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn reopened_context_rejects_the_previous_engines_result() {
        let dir = tempfile::tempdir().unwrap();
        let root = ridge_manifest::canonicalize(dir.path()).unwrap();
        std::fs::write(
            root.join("ridge.toml"),
            "[workspace]\nname = \"owner\"\nversion = \"0.1.0\"\nmembers = []\n",
        )
        .unwrap();
        let file = root.join("scratch.ridge");
        std::fs::write(&file, "pub fn value -> Int = 1\n").unwrap();
        let uri = Url::from_file_path(file).unwrap();
        let mut snap = WorkspaceSnapshot {
            workspaces: vec![Workspace::new(None)],
            ..WorkspaceSnapshot::default()
        };
        snap.open_docs.insert(uri.clone(), String::new());
        snap.document_contexts
            .insert(uri.clone(), DocumentContext::discover(&uri));
        refresh(&mut snap);
        let slot = snap
            .workspaces
            .iter()
            .position(|ws| ws.key.as_ref() == Some(&root))
            .unwrap();
        let previous = Arc::clone(&snap.workspaces[slot].engine);
        assert!(snap.workspaces[slot].accepts_result(&previous, 1));
        snap.open_docs.remove(&uri);
        close(&mut snap, &uri);
        assert!(!snap.workspaces[slot].active);
        snap.open_docs.insert(uri.clone(), String::new());
        snap.document_contexts
            .insert(uri.clone(), DocumentContext::discover(&uri));
        refresh(&mut snap);
        assert_eq!(snap.workspaces.len(), 2, "reopening reuses the stable slot");
        assert!(snap.workspaces[slot].active);
        assert!(
            !snap.workspaces[slot].accepts_result(&previous, u64::MAX),
            "even a late result cannot resurrect the old analysis"
        );
        assert!(snap.workspaces[slot].accepts_result(&snap.workspaces[slot].engine, 1));
    }

    /// Write one policy-owning workspace with two independent source modules.
    fn owner_files(root: &std::path::Path) -> (Url, Url) {
        let folder = root.join("app/src");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            root.join("ridge.toml"),
            "[workspace]\nname = \"owner\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
        )
        .unwrap();
        std::fs::write(root.join("app/ridge.toml"), "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"library\"\n[capabilities]\nallow = []\n").unwrap();
        let first = folder.join("Main.ridge");
        let second = folder.join("Keep.ridge");
        std::fs::write(&first, "pub fn value -> Int = 1\n").unwrap();
        std::fs::write(&second, "pub fn keep -> Int = 2\n").unwrap();
        (
            Url::from_file_path(first).unwrap(),
            Url::from_file_path(second).unwrap(),
        )
    }

    /// Wait for a buffer to appear in the installed index, not just in an engine.
    async fn wait_for_source(server: &super::super::RidgeLanguageServer, uri: &Url, source: &str) {
        for _ in 0..120 {
            if server
                .index_for_uri(uri)
                .await
                .is_some_and(|index| index.module_text.iter().any(|text| text.as_ref() == source))
            {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(
            server
                .index_for_uri(uri)
                .await
                .is_some_and(|index| index.module_text.iter().any(|text| text.as_ref() == source)),
            "the installed analysis never received the current buffer: {uri}"
        );
    }

    /// Retain an engine off the async executor until its test releases the channel.
    fn hold_engine(
        engine: super::super::SharedEngine,
    ) -> (std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>) {
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let blocker = std::thread::spawn(move || {
            let _guard = engine.lock().unwrap();
            held_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(std::time::Duration::from_secs(10));
        });
        held_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        (release_tx, blocker)
    }

    /// Get the retained engine by the physical workspace identity.
    async fn engine_for(
        server: &super::super::RidgeLanguageServer,
        root: &std::path::Path,
    ) -> super::super::SharedEngine {
        let snap = server.state.lock().await;
        Arc::clone(
            &snap
                .workspaces
                .iter()
                .find(|ws| ws.key.as_deref() == Some(root))
                .unwrap()
                .engine,
        )
    }

    #[tokio::test]
    async fn closing_one_context_preserves_another_contexts_pending_buffer() {
        use tower_lsp::lsp_types::{DidCloseTextDocumentParams, TextDocumentIdentifier};
        use tower_lsp::LanguageServer;

        let dir = tempfile::tempdir().unwrap();
        let root = ridge_manifest::canonicalize(dir.path()).unwrap();
        let (a, keep_a) = owner_files(&root.join("a"));
        let (b, _) = owner_files(&root.join("b"));
        let (service, _socket) = tower_lsp::LspService::new(super::super::RidgeLanguageServer::new);
        let server = service.inner();
        let original = "pub fn value -> Int = 1\n";
        {
            let mut snap = server.state.lock().await;
            for uri in [&a, &keep_a, &b] {
                let source = std::fs::read_to_string(uri.to_file_path().unwrap()).unwrap();
                snap.open_docs.insert(uri.clone(), source);
                snap.document_contexts
                    .insert(uri.clone(), DocumentContext::discover(uri));
            }
            refresh(&mut snap);
        }
        server.trigger_compile().await;
        wait_for_source(server, &a, original).await;
        wait_for_source(server, &b, original).await;
        // Hold B's engine on a separate thread. Its compile cannot install before
        // closing A cancels the old controller; the channel releases it on panic too.
        let (release_tx, blocker) = hold_engine(engine_for(server, &root.join("b")).await);
        let edited = "pub fn value -> Int = 9\n";
        server
            .state
            .lock()
            .await
            .open_docs
            .insert(b.clone(), edited.to_owned());
        server
            .run_compile(false, vec![(b.clone(), edited.to_owned())], None)
            .await;
        server
            .did_close(DidCloseTextDocumentParams {
                text_document: TextDocumentIdentifier { uri: a },
            })
            .await;
        release_tx.send(()).unwrap();
        blocker.join().unwrap();
        wait_for_source(server, &b, edited).await;
        assert_eq!(
            std::fs::read_to_string(b.to_file_path().unwrap()).unwrap(),
            original,
            "the analysis must use B's unsaved buffer"
        );
    }

    #[tokio::test]
    async fn incremental_replacement_keeps_a_pending_workspace_policy_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let root = ridge_manifest::canonicalize(dir.path()).unwrap();
        let (a, _) = owner_files(&root.join("a"));
        let (b, _) = owner_files(&root.join("b"));
        let manifest = root.join("b/app/ridge.toml");
        let allowed = "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"library\"\n[capabilities]\nallow = [\"io\"]\n";
        std::fs::write(&manifest, allowed).unwrap();
        let source = "import std.io as Io\npub fn io action () -> Unit = Io.println \"policy\"\n";
        std::fs::write(b.to_file_path().unwrap(), source).unwrap();
        let (service, _socket) = tower_lsp::LspService::new(super::super::RidgeLanguageServer::new);
        let server = service.inner();
        {
            let mut snap = server.state.lock().await;
            for uri in [&a, &b] {
                snap.open_docs.insert(
                    uri.clone(),
                    std::fs::read_to_string(uri.to_file_path().unwrap()).unwrap(),
                );
                snap.document_contexts
                    .insert(uri.clone(), DocumentContext::discover(uri));
            }
            refresh(&mut snap);
        }
        server.trigger_compile().await;
        wait_for_source(server, &b, source).await;
        let baseline = server.index_for_uri(&b).await.unwrap().generation;
        // A comes first in the batch. Retaining its engine prevents the old full
        // controller from starting B before the incremental request replaces it.
        let (release_tx, blocker) = hold_engine(engine_for(server, &root.join("a")).await);
        std::fs::write(&manifest, allowed.replace("[\"io\"]", "[]")).unwrap();
        server.trigger_compile().await;
        let edited = "pub fn value -> Int = 7\n";
        server
            .state
            .lock()
            .await
            .open_docs
            .insert(a.clone(), edited.to_owned());
        server
            .run_compile(false, vec![(a.clone(), edited.to_owned())], None)
            .await;
        release_tx.send(()).unwrap();
        blocker.join().unwrap();
        wait_for_source(server, &a, edited).await;
        for _ in 0..120 {
            if server
                .index_for_uri(&b)
                .await
                .is_some_and(|index| index.generation > baseline)
            {
                let engine = engine_for(server, &root.join("b")).await;
                let guard = engine.lock().unwrap();
                assert!(
                    guard
                        .as_ref()
                        .unwrap()
                        .resolved
                        .errors
                        .iter()
                        .any(|(_, error)| error.code() == "R016"),
                    "the replacement must reload B's now-denied io policy"
                );
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(
            server
                .index_for_uri(&b)
                .await
                .is_some_and(|index| index.generation > baseline),
            "B's full check must survive the replacement"
        );
    }
}
