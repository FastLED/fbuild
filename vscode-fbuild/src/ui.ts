import * as vscode from "vscode";

/** Labels shown in the quick-pick and status bar. */
const BUILD_PROFILES: Record<string, string> = {
  quick: "$(zap) Quick",
  release: "$(package) Release",
  debug: "$(bug) Debug",
};

const ACTIONS: Record<string, string> = {
  "build+deploy+monitor": "$(rocket) Build + Deploy + Monitor",
  build: "$(tools) Build Only",
  deploy: "$(cloud-upload) Deploy Only",
};

/**
 * Manages the status-bar items that let the user pick build profile, action,
 * environment, toggle monitor, and trigger the build.
 */
export class StatusBarUI {
  private profileItem: vscode.StatusBarItem;
  private actionItem: vscode.StatusBarItem;
  private monitorItem: vscode.StatusBarItem;
  private envItem: vscode.StatusBarItem;
  private goItem: vscode.StatusBarItem;

  constructor(ctx: vscode.ExtensionContext) {
    // --- Build profile selector (leftmost) ---
    this.profileItem = vscode.window.createStatusBarItem(
      vscode.StatusBarAlignment.Left,
      100
    );
    this.profileItem.command = "fbuild.selectBuildProfile";
    this.profileItem.tooltip = "fbuild: Build Profile";
    ctx.subscriptions.push(this.profileItem);

    // --- Action selector ---
    this.actionItem = vscode.window.createStatusBarItem(
      vscode.StatusBarAlignment.Left,
      99
    );
    this.actionItem.command = "fbuild.selectAction";
    this.actionItem.tooltip = "fbuild: Action";
    ctx.subscriptions.push(this.actionItem);

    // --- Environment selector ---
    this.envItem = vscode.window.createStatusBarItem(
      vscode.StatusBarAlignment.Left,
      98
    );
    this.envItem.command = "fbuild.selectEnvironment";
    this.envItem.tooltip = "fbuild: Target Environment";
    ctx.subscriptions.push(this.envItem);

    // --- Monitor toggle ---
    this.monitorItem = vscode.window.createStatusBarItem(
      vscode.StatusBarAlignment.Left,
      97
    );
    this.monitorItem.command = "fbuild.toggleMonitor";
    this.monitorItem.tooltip = "fbuild: Attach Monitor on Deploy";
    ctx.subscriptions.push(this.monitorItem);

    // --- Go! button (rightmost of our group) ---
    this.goItem = vscode.window.createStatusBarItem(
      vscode.StatusBarAlignment.Left,
      96
    );
    this.goItem.command = "fbuild.go";
    this.goItem.text = "$(play) Go!";
    this.goItem.tooltip = "fbuild: Execute selected action";
    this.goItem.backgroundColor = new vscode.ThemeColor(
      "statusBarItem.warningBackground"
    );
    ctx.subscriptions.push(this.goItem);

    // Listen for config changes to keep status bar in sync
    ctx.subscriptions.push(
      vscode.workspace.onDidChangeConfiguration((e) => {
        if (e.affectsConfiguration("fbuild")) {
          this.refresh();
        }
      })
    );

    this.refresh();
    this.showAll();
  }

  // ── Quick-pick menus ──────────────────────────────────────────

  async pickBuildProfile(): Promise<void> {
    const items = Object.entries(BUILD_PROFILES).map(([value, label]) => ({
      label,
      value,
    }));

    const picked = await vscode.window.showQuickPick(items, {
      placeHolder: "Select build profile",
    });

    if (picked) {
      await vscode.workspace
        .getConfiguration("fbuild")
        .update(
          "buildProfile",
          picked.value,
          vscode.ConfigurationTarget.Workspace
        );
    }
  }

  async pickAction(): Promise<void> {
    const items = Object.entries(ACTIONS).map(([value, label]) => ({
      label,
      value,
    }));

    const picked = await vscode.window.showQuickPick(items, {
      placeHolder: "Select action",
    });

    if (picked) {
      await vscode.workspace
        .getConfiguration("fbuild")
        .update(
          "action",
          picked.value,
          vscode.ConfigurationTarget.Workspace
        );
    }
  }

  async toggleMonitor(): Promise<void> {
    const config = vscode.workspace.getConfiguration("fbuild");
    const current = config.get<boolean>("attachMonitor", true);
    await config.update(
      "attachMonitor",
      !current,
      vscode.ConfigurationTarget.Workspace
    );
  }

  async pickEnvironment(): Promise<void> {
    const env = await vscode.window.showInputBox({
      prompt:
        "Enter target environment from platformio.ini (e.g. uno, esp32c6). Leave empty for auto-detect.",
      value: vscode.workspace
        .getConfiguration("fbuild")
        .get<string>("environment", ""),
      placeHolder: "auto-detect",
    });

    if (env !== undefined) {
      await vscode.workspace
        .getConfiguration("fbuild")
        .update(
          "environment",
          env,
          vscode.ConfigurationTarget.Workspace
        );
    }
  }

  // ── Status bar rendering ──────────────────────────────────────

  private refresh(): void {
    const config = vscode.workspace.getConfiguration("fbuild");

    const profile = config.get<string>("buildProfile", "release");
    this.profileItem.text =
      BUILD_PROFILES[profile] ?? `$(package) ${profile}`;

    const action = config.get<string>("action", "build+deploy+monitor");
    this.actionItem.text = ACTIONS[action] ?? `$(gear) ${action}`;

    const env = config.get<string>("environment", "");
    this.envItem.text = env ? `$(circuit-board) ${env}` : "$(circuit-board) auto";

    const monitor = config.get<boolean>("attachMonitor", true);
    this.monitorItem.text = monitor
      ? "$(terminal) Monitor: ON"
      : "$(terminal) Monitor: OFF";
  }

  private showAll(): void {
    this.profileItem.show();
    this.actionItem.show();
    this.envItem.show();
    this.monitorItem.show();
    this.goItem.show();
  }
}
