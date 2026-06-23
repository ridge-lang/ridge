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

// Search the canonical cargo bin directory, then PATH, for `binName`. VS Code's
// extension host inherits PATH from whatever launched it (Explorer, Start Menu,
// terminal); that PATH does not always include `~/.cargo/bin` even when the
// user's shell does, so the cargo dir is tried first. Node's spawn() does not
// consult PATHEXT on Windows, so the absolute path is resolved here.
function findExecutable(binName: string): string | undefined {
  const cargoBin = path.join(os.homedir(), ".cargo", "bin", binName);
  if (fs.existsSync(cargoBin)) {
    return cargoBin;
  }
  const pathDirs = (process.env.PATH ?? "")
    .split(path.delimiter)
    .filter((d) => d.length > 0);
  for (const dir of pathDirs) {
    const candidate = path.join(dir, binName);
    if (fs.existsSync(candidate)) {
      return candidate;
    }
  }
  return undefined;
}

// Resolve the ridge-lsp binary: a user-configured override first, then the
// shared cargo/PATH search, then the bare name so spawn fails with a clear error.
function resolveRidgeLsp(): string {
  const binName = process.platform === "win32" ? "ridge-lsp.exe" : "ridge-lsp";
  const configured = vscode.workspace
    .getConfiguration("ridge")
    .get<string>("lspPath");
  if (configured && configured.trim().length > 0 && fs.existsSync(configured)) {
    return configured;
  }
  return findExecutable(binName) ?? binName;
}

// The `ridge` CLI backing the Run / Run-test code lenses, resolved like the
// server binary; anyone who installed ridge-lsp has `ridge` in the same place.
function resolveRidgeCli(): string {
  const binName = process.platform === "win32" ? "ridge.exe" : "ridge";
  return findExecutable(binName) ?? binName;
}

// Quote a shell argument when it contains whitespace or quotes, so a test
// display name like `adds one` or a path with spaces survives `sendText`.
function shellQuote(arg: string): string {
  return /[\s"]/.test(arg) ? `"${arg.replace(/"/g, '\\"')}"` : arg;
}

// Run a Ridge CLI invocation in a fresh integrated terminal opened at the
// workspace root (the terminal's default cwd), where `ridge run`/`ridge test`
// expect to be launched.
function runInTerminal(name: string, commandLine: string): void {
  const terminal = vscode.window.createTerminal({ name });
  terminal.show();
  terminal.sendText(commandLine);
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
    // Opt into code lenses. The server serves none unless a client asks, so
    // editors that can't run the lens commands never see inert lenses.
    initializationOptions: {
      codeLens: {
        references: true,
        implementations: true,
        run: true,
        runTest: true,
      },
    },
    middleware: {
      // The navigational lenses ("N references" / "N implementations") carry the
      // built-in `editor.action.showReferences`, but its arguments arrive as JSON
      // across the protocol and the built-in needs live vscode objects. Rehydrate
      // them after the server resolves the lens.
      resolveCodeLens: async (codeLens, token, next) => {
        const resolved = await next(codeLens, token);
        const command = resolved?.command;
        if (
          command &&
          command.command === "editor.action.showReferences" &&
          Array.isArray(command.arguments)
        ) {
          const [uri, position, locations] = command.arguments as [
            string,
            { line: number; character: number },
            Array<{
              uri: string;
              range: {
                start: { line: number; character: number };
                end: { line: number; character: number };
              };
            }>,
          ];
          const toPos = (p: { line: number; character: number }) =>
            new vscode.Position(p.line, p.character);
          command.arguments = [
            vscode.Uri.parse(uri),
            toPos(position),
            locations.map(
              (l) =>
                new vscode.Location(
                  vscode.Uri.parse(l.uri),
                  new vscode.Range(toPos(l.range.start), toPos(l.range.end)),
                ),
            ),
          ];
        }
        return resolved;
      },
    },
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

  // Client-side commands the Run / Run-test code lenses invoke. The server emits
  // the command + arguments; running the CLI in a terminal is the editor's job.
  context.subscriptions.push(
    vscode.commands.registerCommand("ridge.run", (project?: string) => {
      const ridge = shellQuote(resolveRidgeCli());
      const member =
        project && project.length > 0 ? ` --member ${shellQuote(project)}` : "";
      runInTerminal("Ridge Run", `${ridge} run${member}`);
    }),
    vscode.commands.registerCommand(
      "ridge.test",
      (project?: string, test?: string) => {
        const ridge = shellQuote(resolveRidgeCli());
        const member =
          project && project.length > 0
            ? ` --member ${shellQuote(project)}`
            : "";
        // The CLI matches `--filter` as a glob against `Module.<display-name>`,
        // so anchor on the display name with a leading wildcard for the module.
        const filter =
          test && test.length > 0 ? ` --filter ${shellQuote(`*.${test}`)}` : "";
        runInTerminal("Ridge Test", `${ridge} test${member}${filter}`);
      },
    ),
  );
}

export async function deactivate(): Promise<void> {
  if (client) {
    await client.stop();
  }
}
