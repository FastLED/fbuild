import * as vscode from "vscode";

/** Labels shown in the quick-pick menus. */
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

interface ConfigItem extends vscode.QuickPickItem {
  id: string;
}

/**
 * Two status-bar items that read as one unified group:
 *   [ ⚙ release | auto | monitor ][ ▶ Go! ]
 *
 * Clicking the config item opens a quick-pick to change any setting.
 * Clicking Go! runs the selected action.
 */
export class StatusBarUI {
  private configItem: vscode.StatusBarItem;
  private goItem: vscode.StatusBarItem;

  constructor(ctx: vscode.ExtensionContext) {
    // --- Config summary (left half of the group) ---
    this.configItem = vscode.window.createStatusBarItem(
      vscode.StatusBarAlignment.Left,
      100
    );
    this.configItem.command = "fbuild.configure";
    this.configItem.tooltip = "fbuild: Click to configure";
    ctx.subscriptions.push(this.configItem);

    // --- Go! button (right half of the group) ---
    this.goItem = vscode.window.createStatusBarItem(
      vscode.StatusBarAlignment.Left,
      99
    );
    this.goItem.command = "fbuild.go";
    this.goItem.text = "$(play) Go!";
    this.goItem.tooltip = "fbuild: Execute selected action";
    this.goItem.backgroundColor = new vscode.ThemeColor(
      "statusBarItem.warningBackground"
    );
    ctx.subscriptions.push(this.goItem);

    // Keep in sync with config changes
    ctx.subscriptions.push(
      vscode.workspace.onDidChangeConfiguration((e) => {
        if (e.affectsConfiguration("fbuild")) {
          this.refresh();
        }
      })
    );

    this.refresh();
    this.configItem.show();
    this.goItem.show();
  }

  // ── Combined configure menu ───────────────────────────────────

  async configure(): Promise<void> {
    const config = vscode.workspace.getConfiguration("fbuild");
    const profile = config.get<string>("buildProfile", "release");
    const action = config.get<string>("action", "build+deploy+monitor");
    const monitor = config.get<boolean>("attachMonitor", true);
    const env = config.get<string>("environment", "") || "auto";

    const items: ConfigItem[] = [
      {
        label: "$(package) Build Profile",
        description: profile,
        id: "profile",
      },
      {
        label: "$(rocket) Action",
        description: action,
        id: "action",
      },
      {
        label: "$(circuit-board) Environment",
        description: env,
        id: "environment",
      },
      {
        label: "$(terminal) Monitor",
        description: monitor ? "ON" : "OFF",
        id: "monitor",
      },
    ];

    const picked = await vscode.window.showQuickPick(items, {
      placeHolder: "Configure fbuild",
    });

    if (picked) {
      switch (picked.id) {
        case "profile":
          await this.pickBuildProfile();
          break;
        case "action":
          await this.pickAction();
          break;
        case "environment":
          await this.pickEnvironment();
          break;
        case "monitor":
          await this.toggleMonitor();
          break;
      }
    }
  }

  // ── Individual quick-pick menus ───────────────────────────────

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
    const env = config.get<string>("environment", "") || "auto";
    const monitor = config.get<boolean>("attachMonitor", true);

    const monitorLabel = monitor ? "monitor" : "no-monitor";
    this.configItem.text =
      `$(gear) fbuild: ${profile} | ${env} | ${monitorLabel}`;
  }
}
