// The Kāra extension: a thin client over `kara-lsp` (lsp/ in this repo).
//
// Everything language-aware (diagnostics, hover, go-to-definition,
// references, outline, formatting, quick fixes) comes from the server; this
// file only finds the binary, starts it over stdio and restarts it on demand.
// Syntax highlighting is the TextMate grammar in syntaxes/, which works with
// or without a server.

import * as fs from "fs";
import * as path from "path";
import * as vscode from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  TransportKind,
} from "vscode-languageclient/node";

let client: LanguageClient | undefined;
let output: vscode.OutputChannel | undefined;

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  output = vscode.window.createOutputChannel("Kāra Language Server");
  context.subscriptions.push(output);

  context.subscriptions.push(
    vscode.commands.registerCommand("kara.restartServer", async () => {
      await stopClient();
      await startClient(context);
    }),
    vscode.commands.registerCommand("kara.showOutput", () => output?.show(true)),
    vscode.workspace.onDidChangeConfiguration(async (e) => {
      if (e.affectsConfiguration("kara.server")) {
        await stopClient();
        await startClient(context);
      }
    }),
  );

  await startClient(context);
}

export async function deactivate(): Promise<void> {
  await stopClient();
}

async function startClient(context: vscode.ExtensionContext): Promise<void> {
  const config = vscode.workspace.getConfiguration("kara");
  const command = resolveServer(context, config.get<string>("server.path", ""));
  if (command === undefined) {
    const pick = await vscode.window.showWarningMessage(
      "Kāra: kara-lsp was not found, so there are no diagnostics or go-to-definition " +
        "(highlighting still works). Install it with `cargo install --path lsp` from a " +
        "kara checkout, or set `kara.server.path`.",
      "Open Settings",
    );
    if (pick === "Open Settings") {
      await vscode.commands.executeCommand("workbench.action.openSettings", "kara.server.path");
    }
    return;
  }
  output?.appendLine(`Starting ${command}`);

  const extraEnv = config.get<Record<string, string>>("server.extraEnv", {});
  const run = {
    command,
    transport: TransportKind.stdio,
    options: { env: { ...process.env, ...extraEnv } },
  };
  const serverOptions: ServerOptions = { run, debug: run };
  const clientOptions: LanguageClientOptions = {
    documentSelector: [
      { scheme: "file", language: "kara" },
      { scheme: "untitled", language: "kara" },
    ],
    outputChannel: output,
    traceOutputChannel: output,
  };

  client = new LanguageClient("kara", "Kāra Language Server", serverOptions, clientOptions);
  try {
    await client.start();
  } catch (err) {
    output?.appendLine(`kara-lsp failed to start: ${err}`);
    void vscode.window.showErrorMessage(
      `Kāra: kara-lsp failed to start (${command}). See the "Kāra Language Server" output.`,
    );
    client = undefined;
  }
}

async function stopClient(): Promise<void> {
  const c = client;
  client = undefined;
  if (c !== undefined) {
    await c.stop();
  }
}

// The server binary, in order: the `kara.server.path` setting, a binary
// bundled in the extension's `server/` folder (platform builds of the .vsix),
// then `kara-lsp` on PATH. Undefined when none exists.
function resolveServer(context: vscode.ExtensionContext, configured: string): string | undefined {
  const exe = process.platform === "win32" ? "kara-lsp.exe" : "kara-lsp";
  if (configured !== "") {
    const expanded = configured.replace(/^~(?=$|[\\/])/, process.env.HOME ?? "~");
    return isFile(expanded) ? expanded : undefined;
  }
  const bundled = context.asAbsolutePath(path.join("server", exe));
  if (isFile(bundled)) {
    return bundled;
  }
  return findOnPath(exe);
}

function findOnPath(exe: string): string | undefined {
  const dirs = (process.env.PATH ?? "").split(path.delimiter);
  // `cargo install` puts binaries in ~/.cargo/bin, which a GUI-launched
  // VS Code on macOS often does not have on its PATH.
  const home = process.env.HOME ?? process.env.USERPROFILE;
  if (home !== undefined) {
    dirs.push(path.join(home, ".cargo", "bin"));
  }
  for (const dir of dirs) {
    if (dir === "") {
      continue;
    }
    const candidate = path.join(dir, exe);
    if (isFile(candidate)) {
      return candidate;
    }
  }
  return undefined;
}

function isFile(p: string): boolean {
  try {
    return fs.statSync(p).isFile();
  } catch {
    return false;
  }
}
