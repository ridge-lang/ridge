import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import * as vscode from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  TransportKind,
} from "vscode-languageclient/node";

let client: LanguageClient | undefined;

// Resolve the ridge-lsp binary path. VS Code's extension host inherits PATH
// from whatever process launched it (Explorer, Start Menu, terminal); that
// PATH does not always include `~/.cargo/bin` even when the user's shell
// does. We try a chain of candidates and pick the first that exists.
function resolveRidgeLsp(): string {
  const isWindows = process.platform === "win32";
  const binName = isWindows ? "ridge-lsp.exe" : "ridge-lsp";

  // 1. User-configured override.
  const configured = vscode.workspace
    .getConfiguration("ridge")
    .get<string>("lspPath");
  if (configured && configured.trim().length > 0 && fs.existsSync(configured)) {
    return configured;
  }

  // 2. Canonical `cargo install` destination — always populated by
  //    `tools/install/install.{sh,ps1}` on every supported platform.
  const cargoBin = path.join(os.homedir(), ".cargo", "bin", binName);
  if (fs.existsSync(cargoBin)) {
    return cargoBin;
  }

  // 3. Walk PATH manually. Node's spawn() does not consult PATHEXT on
  //    Windows, so we resolve the absolute path ourselves and pass that.
  const pathDirs = (process.env.PATH ?? "")
    .split(path.delimiter)
    .filter((d) => d.length > 0);
  for (const dir of pathDirs) {
    const candidate = path.join(dir, binName);
    if (fs.existsSync(candidate)) {
      return candidate;
    }
  }

  // 4. Fall back to bare name and let spawn fail with a useful error.
  return binName;
}

export function activate(context: vscode.ExtensionContext): void {
  const ridgeLsp = resolveRidgeLsp();

  const serverOptions: ServerOptions = {
    command: ridgeLsp,
    args: [],
    transport: TransportKind.stdio,
  };

  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ scheme: "file", language: "ridge" }],
  };

  client = new LanguageClient(
    "ridge-lsp",
    "Ridge Language Server",
    serverOptions,
    clientOptions,
  );

  // Start the client. If ridge-lsp cannot be spawned the client emits an
  // error event — catch it and surface a friendly message that names the
  // path we actually tried.
  client.start().catch((err: unknown) => {
    const hint =
      `Tried to spawn: ${ridgeLsp}. ` +
      "Install via `tools/install/install.sh` (Linux/macOS) or " +
      "`tools/install/install.ps1` (Windows), or set the " +
      "`ridge.lspPath` setting to an absolute path. Reload VS Code after fixing.";
    vscode.window.showErrorMessage(
      `Ridge: failed to start language server. ${hint} (${String(err)})`,
    );
  });

  context.subscriptions.push(client);
}

export async function deactivate(): Promise<void> {
  if (client) {
    await client.stop();
  }
}
