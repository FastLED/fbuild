import * as vscode from "vscode";
import { FbuildRunner } from "./fbuildRunner";
import { StatusBarUI } from "./ui";

let runner: FbuildRunner;
let ui: StatusBarUI;

export function activate(context: vscode.ExtensionContext): void {
  runner = new FbuildRunner();
  ui = new StatusBarUI(context);

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
      ui.pickEnvironment()
    )
  );
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
