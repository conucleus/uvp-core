import { copyFile, mkdir } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const packageDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const workspaceDir = resolve(packageDir, "../..");
const release = process.argv.includes("--release");
const profile = release ? "release" : "debug";
const sourceName = nativeLibraryName(process.platform);
const source = resolve(workspaceDir, "target", profile, sourceName);
const destination = resolve(packageDir, "uvp_node.node");
const args = ["build", "--manifest-path", resolve(workspaceDir, "Cargo.toml"), "-p", "uvp-node"];
if (release) {
  args.push("--release");
}

const result = spawnSync("cargo", args, { cwd: workspaceDir, stdio: "inherit" });
if (result.error) {
  throw result.error;
}
if (result.status !== 0) {
  process.exit(result.status ?? 1);
}

await mkdir(packageDir, { recursive: true });
await copyFile(source, destination);

function nativeLibraryName(platform) {
  switch (platform) {
    case "darwin":
      return "libuvp_node.dylib";
    case "linux":
      return "libuvp_node.so";
    case "win32":
      return "uvp_node.dll";
    default:
      throw new Error(`unsupported uvp-core Node platform: ${platform}`);
  }
}
