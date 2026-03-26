import * as vscode from "vscode";

const BUILD_PROFILES: Record<string, string> = {
  quick: "Quick",
  release: "Release",
  debug: "Debug",
};

const ACTIONS: Record<string, string> = {
  "build+deploy+monitor": "Build + Deploy + Monitor",
  build: "Build Only",
  deploy: "Deploy Only",
};

type ConfigItemId = "profile" | "action" | "environment" | "monitor";

interface ConfigItemDef {
  id: ConfigItemId;
  label: string;
  icon: string;
  command: string;
}

const CONFIG_ITEMS: ConfigItemDef[] = [
  {
    id: "profile",
    label: "Profile",
    icon: "package",
    command: "fbuild.selectBuildProfile",
  },
  {
    id: "action",
    label: "Action",
    icon: "rocket",
    command: "fbuild.selectAction",
  },
  {
    id: "environment",
    label: "Environment",
    icon: "circuit-board",
    command: "fbuild.selectEnvironment",
  },
  {
    id: "monitor",
    label: "Monitor",
    icon: "terminal",
    command: "fbuild.toggleMonitor",
  },
];

/**
 * Tree data provider for the fbuild sidebar panel.
 *
 * Shows the current build configuration (profile, action, environment, monitor)
 * as clickable tree items, plus a "Go!" action item at the bottom.
 */
export class FbuildTreeProvider
  implements vscode.TreeDataProvider<vscode.TreeItem>
{
  private _onDidChangeTreeData = new vscode.EventEmitter<void>();
  readonly onDidChangeTreeData = this._onDidChangeTreeData.event;

  constructor(ctx: vscode.ExtensionContext) {
    ctx.subscriptions.push(
      vscode.workspace.onDidChangeConfiguration((e) => {
        if (e.affectsConfiguration("fbuild")) {
          this._onDidChangeTreeData.fire();
        }
      })
    );
  }

  refresh(): void {
    this._onDidChangeTreeData.fire();
  }

  getTreeItem(element: vscode.TreeItem): vscode.TreeItem {
    return element;
  }

  getChildren(element?: vscode.TreeItem): vscode.TreeItem[] {
    if (element) {
      return [];
    }

    const config = vscode.workspace.getConfiguration("fbuild");
    const items: vscode.TreeItem[] = [];

    for (const def of CONFIG_ITEMS) {
      const item = new vscode.TreeItem(
        def.label,
        vscode.TreeItemCollapsibleState.None
      );
      item.iconPath = new vscode.ThemeIcon(def.icon);
      item.command = { command: def.command, title: def.label };

      switch (def.id) {
        case "profile": {
          const val = config.get<string>("buildProfile", "release");
          item.description = BUILD_PROFILES[val] ?? val;
          break;
        }
        case "action": {
          const val = config.get<string>("action", "build+deploy+monitor");
          item.description = ACTIONS[val] ?? val;
          break;
        }
        case "environment": {
          const val = config.get<string>("environment", "");
          item.description = val || "auto-detect";
          break;
        }
        case "monitor": {
          const val = config.get<boolean>("attachMonitor", true);
          item.description = val ? "ON" : "OFF";
          item.iconPath = new vscode.ThemeIcon(val ? "eye" : "eye-closed");
          break;
        }
      }

      items.push(item);
    }

    // "Go!" action item
    const goItem = new vscode.TreeItem(
      "Go!",
      vscode.TreeItemCollapsibleState.None
    );
    goItem.iconPath = new vscode.ThemeIcon("play");
    goItem.command = { command: "fbuild.go", title: "Go!" };
    goItem.tooltip = "Execute the selected action";
    items.push(goItem);

    return items;
  }
}

/**
 * Parse environment names from a platformio.ini file.
 *
 * Looks for `[env:name]` sections and returns the list of names.
 */
export async function detectEnvironments(): Promise<string[]> {
  const files = await vscode.workspace.findFiles(
    "platformio.ini",
    undefined,
    1
  );
  if (files.length === 0) {
    return [];
  }

  const doc = await vscode.workspace.openTextDocument(files[0]);
  const text = doc.getText();
  const envPattern = /^\[env:([^\]]+)\]/gm;
  const envs: string[] = [];
  let match: RegExpExecArray | null;

  while ((match = envPattern.exec(text)) !== null) {
    envs.push(match[1]);
  }

  return envs;
}
