import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const imageName = process.env.FNFYU_HARNESS_IMAGE ?? "local-first-harness:dev";

function usage(commandName) {
  console.log(`用法：
  ${commandName} web [--build|--no-build]
  ${commandName} --help

说明：
  ${commandName} web       启动 fnfyu harness Web 网关，镜像不存在时自动构建
  --build                  强制同步 Docker 镜像
  --no-build               跳过镜像检查与构建，直接启动网关

环境变量：
  HARNESS_PROJECT_ROOT     覆盖项目根目录
  HARNESS_WORKSPACE        覆盖要挂载到运行时的工作区
  FNFYU_HARNESS_IMAGE      覆盖 Docker 镜像名`);
}

function shellQuote(value) {
  return `'${String(value).replaceAll("'", "'\\''")}'`;
}

function toWslPath(value) {
  const match = value.match(/^([a-zA-Z]):[\\/](.*)$/);
  if (!match) return value.replaceAll("\\", "/");
  return `/mnt/${match[1].toLowerCase()}/${match[2].replaceAll("\\", "/")}`;
}

function run(commandName, command, args, options = {}) {
  return new Promise((resolveRun) => {
    const child = spawn(command, args, { stdio: "inherit", ...options });
    child.once("error", (error) => {
      console.error(`${commandName}: 无法启动 ${command}：${error.message}`);
      resolveRun(1);
    });
    child.once("exit", (code, signal) => {
      const exitCode = typeof code === "number" ? code : signal ? 1 : 0;
      if (exitCode !== 0) console.error(`${commandName}: 网关进程已退出（代码 ${exitCode}）`);
      resolveRun(exitCode);
    });
  });
}

async function runDockerWeb(commandName, { noBuild, forceBuild }) {
  const root = process.env.HARNESS_PROJECT_ROOT ?? projectRoot;
  const workspace = process.env.HARNESS_WORKSPACE ?? process.cwd();
  const composeFile = resolve(root, "docker-compose.yml");
  const build = noBuild
    ? ""
    : forceBuild
      ? `echo '${commandName}: 正在同步 fnfyu harness 镜像…'\ndocker compose -f ${shellQuote(toWslPath(composeFile))} build --pull harnessd || exit $?\n`
      : `if ! docker image inspect ${shellQuote(imageName)} >/dev/null 2>&1; then\n  echo '${commandName}: 未找到 fnfyu harness 镜像，正在构建…'\n  docker compose -f ${shellQuote(toWslPath(composeFile))} build --pull harnessd || exit $?\nfi\n`;
  const command = `${build}export HARNESS_PROJECT_ROOT=${shellQuote(toWslPath(root))}\nexport HARNESS_WORKSPACE=${shellQuote(toWslPath(workspace))}\nexec docker compose -f ${shellQuote(toWslPath(composeFile))} --profile web up gateway`;

  if (process.platform === "win32") {
    return run(commandName, "wsl.exe", ["--", "bash", "-lc", `cd ${shellQuote(toWslPath(root))} && ${command}`]);
  }

  return run(commandName, "bash", ["-lc", `cd ${shellQuote(root)} && ${command}`]);
}

export async function main(commandName = "fnfyuh") {
  const [command, ...args] = process.argv.slice(2);
  if (!command || command === "help" || command === "--help" || command === "-h") {
    usage(commandName);
    return command ? 0 : 1;
  }

  if (command !== "web") {
    console.error(`${commandName}: 未知命令“${command}”，请运行 ${commandName} --help 查看用法`);
    return 1;
  }
  const supported = ["--build", "--no-build", "--help", "-h"];
  if (args.some((argument) => !supported.includes(argument))) {
    console.error(`${commandName}: web 只支持 --build 或 --no-build；请运行 ${commandName} web --help 查看用法`);
    return 1;
  }
  if (args.includes("--build") && args.includes("--no-build")) {
    console.error(`${commandName}: --build 和 --no-build 不能同时使用`);
    return 1;
  }
  if (args.includes("--help") || args.includes("-h")) {
    usage(commandName);
    return 0;
  }
  return runDockerWeb(commandName, {
    noBuild: args.includes("--no-build"),
    forceBuild: args.includes("--build"),
  });
}
