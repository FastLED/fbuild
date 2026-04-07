import * as vscode from "vscode";
import { FbuildRunner } from "./fbuildRunner";
import { StatusBarUI } from "./ui";
import { FbuildTreeProvider, detectEnvironments } from "./treeView";
import { ValueQuickPickItem } from "./constants";

let runner: FbuildRunner;
let ui: StatusBarUI;
let treeProvider: FbuildTreeProvider;

export function activate(context: vscode.ExtensionContext): void {
  runner = new FbuildRunner();
  ui = new StatusBarUI(context);
  treeProvider = new FbuildTreeProvider(context);

  // Register the sidebar tree view
  const treeView = vscode.window.createTreeView("fbuild.config", {
    treeDataProvider: treeProvider,
  });
  context.subscriptions.push(treeView);

  // Set context key so welcome view and keybindings know a project is open
  vscode.commands.executeCommand("setContext", "fbuild.projectDetected", true);

  context.subscriptions.push(
    vscode.commands.registerCommand("fbuild.go", () => executeGo()),
    vscode.commands.registerCommand("fbuild.selectBuildProfile", () =>
      ui.pickBuildProfile()
    ),
    vscode.commands.registerCommand("fbuild.selectAction", () =>
      ui.pickAction()
    ),
    vscode.commands.registerCommand("fbuild.toggleMonitor", () =>
      ui.toggleMonitor()
    ),
    vscode.commands.registerCommand("fbuild.selectEnvironment", () =>
      pickEnvironmentFromIni()
    )
  );
}

/**
 * Enhanced environment picker that auto-detects environments from
 * platformio.ini and presents them as quick-pick items.
 */
async function pickEnvironmentFromIni(): Promise<void> {
  const envs = await detectEnvironments();
  const config = vscode.workspace.getConfiguration("fbuild");
  const current = config.get<string>("environment", "");

  if (envs.length > 0) {
    // Show detected environments as quick-pick items
    const items: ValueQuickPickItem[] = [
      {
        label: "$(search) auto-detect",
        description: "Let fbuild choose the environment",
        detail: current === "" ? "Currently selected" : undefined,
        value: "",
      },
      ...envs.map((e) => ({
        label: `$(circuit-board) ${e}`,
        description: e === current ? "Currently selected" : undefined,
        detail: undefined as string | undefined,
        value: e,
      })),
    ];

    const picked = await vscode.window.showQuickPick(items, {
      placeHolder: "Select target environment",
    });

    if (picked) {
      await config.update(
        "environment",
        picked.value,
        vscode.ConfigurationTarget.Workspace
      );
    }
  } else {
    // Fallback to input box if no environments detected
    const env = await vscode.window.showInputBox({
      prompt:
        "Enter target environment from platformio.ini (e.g. uno, esp32c6). Leave empty for auto-detect.",
      value: current,
      placeHolder: "auto-detect",
    });

    if (env !== undefined) {
      await config.update(
        "environment",
        env,
        vscode.ConfigurationTarget.Workspace
      );
    }
  }
}

async function executeGo(): Promise<void> {
  const config = vscode.workspace.getConfiguration("fbuild");
  const profile = config.get<string>("buildProfile", "release");
  const action = config.get<string>("action", "build+deploy+monitor");
  const attachMonitor = config.get<boolean>("attachMonitor", true);
  const environment = config.get<string>("environment", "");

  const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
  if (!workspaceFolder) {
    vscode.window.showErrorMessage(
      "fbuild: No workspace folder open. Open a project folder first."
    );
    return;
  }
  const cwd = workspaceFolder.uri.fsPath;

  switch (action) {
    case "build":
      runner.build(cwd, profile, environment);
      break;
    case "deploy":
      runner.deploy(cwd, environment, attachMonitor);
      break;
    case "build+deploy+monitor":
      runner.buildDeployMonitor(cwd, profile, environment);
      break;
    default:
      vscode.window.showErrorMessage(`fbuild: Unknown action "${action}"`);
  }
}

export function deactivate(): void {
  runner?.dispose();
}
