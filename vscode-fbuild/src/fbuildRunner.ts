import * as vscode from "vscode";
import * as cp from "child_process";

/**
 * Wraps invocations of the `fbuild` CLI tool.
 *
 * fbuild is expected to be installed as a pip package (`pip install fbuild`)
 * and available on the user's PATH (or at the path configured via
 * `fbuild.pythonPath`).
 */
export class FbuildRunner {
  private terminal: vscode.Terminal | undefined;

  /** Build firmware only. */
  build(cwd: string, profile: string, env: string): void {
    const args = this.buildArgs(cwd, profile, env, ["build"]);
    this.runInTerminal("fbuild: Build", args, cwd);
  }

  /** Deploy pre-built firmware to a device. */
  deploy(cwd: string, env: string, attachMonitor: boolean): void {
    const cmd = attachMonitor ? "deploy --monitor" : "deploy";
    const args = this.buildArgs(cwd, "", env, cmd.split(" "));
    this.runInTerminal("fbuild: Deploy", args, cwd);
  }

  /** Build, deploy, and open the serial monitor. */
  buildDeployMonitor(cwd: string, profile: string, env: string): void {
    const buildArgs = this.buildArgs(cwd, profile, env, ["build"]);
    const deployArgs = this.buildArgs(cwd, "", env, ["deploy", "--monitor"]);

    // Chain build then deploy+monitor
    const fullCmd = `${this.fbuildBin()} ${buildArgs.join(" ")} && ${this.fbuildBin()} ${deployArgs.join(" ")}`;
    this.runRawInTerminal("fbuild: Build+Deploy+Monitor", fullCmd, cwd);
  }

  dispose(): void {
    this.terminal?.dispose();
  }

  // ── helpers ──────────────────────────────────────────────────

  private buildArgs(
    cwd: string,
    profile: string,
    env: string,
    command: string[]
  ): string[] {
    const args: string[] = [...command, cwd];

    if (env) {
      args.push("-e", env);
    }

    if (profile && command.includes("build")) {
      switch (profile) {
        case "quick":
          args.push("--quick");
          break;
        case "release":
          args.push("--release");
          break;
        case "debug":
          args.push("--debug");
          break;
      }
    }

    return args;
  }

  private fbuildBin(): string {
    const pythonPath = vscode.workspace
      .getConfiguration("fbuild")
      .get<string>("pythonPath", "");

    if (pythonPath) {
      // Use the python interpreter to run fbuild as a module
      return `"${pythonPath}" -m fbuild`;
    }
    return "fbuild";
  }

  private runInTerminal(name: string, args: string[], cwd: string): void {
    const cmd = `${this.fbuildBin()} ${args.join(" ")}`;
    this.runRawInTerminal(name, cmd, cwd);
  }

  private runRawInTerminal(name: string, cmd: string, cwd: string): void {
    // Reuse or create a terminal
    if (this.terminal) {
      this.terminal.dispose();
    }
    this.terminal = vscode.window.createTerminal({ name, cwd });
    this.terminal.show();
    this.terminal.sendText(cmd);
  }

  /**
   * Check if fbuild is available on the system. Used for diagnostic purposes.
   */
  static checkInstalled(): Promise<boolean> {
    return new Promise((resolve) => {
      cp.exec("fbuild --version", (error) => {
        resolve(!error);
      });
    });
  }
}
