import * as vscode from "vscode";
import {
  BUILD_PROFILES,
  BUILD_PROFILE_ICONS,
  ACTION_ICONS,
  ACTION_SHORT,
  ValueQuickPickItem,
} from "./constants";

/**
 * Manages the status-bar items: a compact configuration summary and a Go! button.
 *
 * The full configuration UI lives in the sidebar tree view (see treeView.ts).
 * The status bar provides a quick-glance summary and one-click execution.
 */
export class StatusBarUI {
  private summaryItem: vscode.StatusBarItem;
  private goItem: vscode.StatusBarItem;

  constructor(ctx: vscode.ExtensionContext) {
    // --- Compact config summary (click opens sidebar) ---
    this.summaryItem = vscode.window.createStatusBarItem(
      vscode.StatusBarAlignment.Left,
      100
    );
    this.summaryItem.command = "workbench.view.extension.fbuild-sidebar";
    this.summaryItem.tooltip =
      "fbuild: Click to open configuration panel";
    ctx.subscriptions.push(this.summaryItem);

    // --- Go! button ---
    this.goItem = vscode.window.createStatusBarItem(
      vscode.StatusBarAlignment.Left,
      99
    );
    this.goItem.command = "fbuild.go";
    this.goItem.text = "$(play) Go!";
    this.goItem.tooltip = "fbuild: Execute selected action (Ctrl+Shift+G)";
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
    this.summaryItem.show();
    this.goItem.show();
  }

  // ── Quick-pick menus (called from commands / tree view clicks) ──

  async pickBuildProfile(): Promise<void> {
    const items: ValueQuickPickItem[] = Object.entries(BUILD_PROFILE_ICONS).map(
      ([value, label]) => ({ label, value })
    );

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
    const items: ValueQuickPickItem[] = Object.entries(ACTION_ICONS).map(
      ([value, label]) => ({ label, value })
    );

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

  // ── Status bar rendering ──────────────────────────────────────

  private refresh(): void {
    const config = vscode.workspace.getConfiguration("fbuild");

    const profile = config.get<string>("buildProfile", "release");
    const action = config.get<string>("action", "build+deploy+monitor");
    const env = config.get<string>("environment", "");

    const profileLabel = BUILD_PROFILES[profile] ?? profile;
    const actionLabel = ACTION_SHORT[action] ?? action;
    const envLabel = env || "auto";

    this.summaryItem.text = `$(zap) ${profileLabel} | ${actionLabel} | ${envLabel}`;
  }
}
