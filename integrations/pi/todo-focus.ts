import { basename } from "node:path";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { StringEnum } from "@earendil-works/pi-ai";
import { Type } from "typebox";

const KEY = "todo-focus";
const TASK_ID = /^[0-9a-f]{32}$/;

type Task = {
  id: string;
  title: string;
  list_title: string;
  pins: { agent: string; avatar: string }[];
};

function clean(text: string, limit = 120): string {
  return Array.from(text.replace(/[\p{Cc}\p{Cf}]+/gu, " ").replace(/\s+/g, " ").trim()).slice(0, limit).join("");
}

export default function (pi: ExtensionAPI) {
  const binary = process.env.PI_TODO_BIN?.trim() || "todo-tui";
  const avatar = clean(process.env.PI_TODO_AVATAR ?? "🛠", 16) || "🛠";
  const identity = (ctx: ExtensionContext) => clean(process.env.PI_TODO_AGENT ?? "", 96) ||
    `Pi/${clean(basename(ctx.cwd), 48) || "work"}/${ctx.sessionManager.getSessionId().slice(0, 8)}`;

  let timer: ReturnType<typeof setInterval> | undefined;
  let session: AbortController | undefined;
  let refreshRevision = 0;
  let polling = false;
  let focusId: string | undefined;
  let lastStatus: string | undefined;

  async function cli<T>(args: string[], signal?: AbortSignal): Promise<T> {
    const result = await pi.exec(binary, ["--json", ...args], { signal, timeout: 3500, maxBuffer: 2 * 1024 * 1024 });
    if (result.code !== 0 || result.killed || result.truncated) {
      throw new Error(clean(result.stderr, 240) || `todo-tui ${args[0]} failed (exit ${result.code})`);
    }
    return JSON.parse(result.stdout) as T;
  }

  const pins = (ctx: ExtensionContext, signal?: AbortSignal) =>
    cli<Task[]>(["pinned", `--agent=${identity(ctx)}`], signal);

  async function refresh(ctx: ExtensionContext): Promise<void> {
    if (!ctx.hasUI || !session) return;
    const signal = session.signal;
    const revision = ++refreshRevision;
    let status: string;
    try {
      const active = await pins(ctx, signal);
      if (signal.aborted || revision !== refreshRevision) return;
      const task = active.find((item) => item.id === focusId) ?? active[0];
      focusId = task?.id;
      const marker = clean(task?.pins.find((p) => p.agent === identity(ctx))?.avatar ?? avatar, 16);
      status = task ? `${marker} Focus: ${clean(task.title, 64)}${active.length > 1 ? ` (+${active.length - 1})` : ""}` : "";
    } catch {
      status = "Task focus unavailable";
    }
    if (!signal.aborted && revision === refreshRevision && status !== lastStatus) {
      lastStatus = status;
      ctx.ui.setStatus(KEY, status || undefined);
    }
  }

  pi.on("session_start", async (_event, ctx) => {
    session = new AbortController();
    await refresh(ctx);
    if (!ctx.hasUI) return;
    timer = setInterval(() => {
      if (polling) return;
      polling = true;
      void refresh(ctx).finally(() => { polling = false; }).catch(() => {});
    }, 2000);
    timer.unref();
  });

  pi.on("session_shutdown", (_event, ctx) => {
    clearInterval(timer);
    timer = undefined;
    session?.abort();
    session = undefined;
    lastStatus = undefined;
    focusId = undefined;
    if (ctx.hasUI) ctx.ui.setStatus(KEY, undefined);
  });

  pi.registerCommand("todo-focus", {
    description: "Show this Pi agent’s pinned todo-tui items",
    async handler(_args, ctx) {
      try {
        const active = await pins(ctx, session?.signal);
        const tasks = active.slice(0, 12).map((task) => `${clean(task.title)} · ${clean(task.list_title, 48)} · ${task.id}`);
        if (active.length > tasks.length) tasks.push(`+${active.length - tasks.length} more`);
        ctx.ui.notify(`${identity(ctx)}\n${tasks.join("\n") || "No pinned items"}`, "info");
        await refresh(ctx);
      } catch (error) {
        ctx.ui.notify(clean(String(error), 240), "warning");
      }
    },
  });

  pi.registerTool({
    name: "todo_tui_focus",
    label: "Todo focus",
    description: "Track substantial work in the user's todo-tui. Start pins an existing ID or creates and pins a concise work item. Status lists this agent's pins. Pause unpins without completing; finish completes a pinned task.",
    promptSnippet: "Create or pin a todo-tui work item before substantial work, and update its status afterwards",
    promptGuidelines: [
      "Before starting substantial implementation, multi-step investigation, or operational work, call todo_tui_focus with action=start in its own tool call before doing that work. Use an existing task ID when the user identified one; otherwise use a short task title. Check status if continuing earlier work, and reuse the relevant pin. Do not create tasks for quick questions, small edits, or chat.",
      "Use todo_tui_focus action=finish only when the tracked task is actually complete; use pause to release unfinished work without completing it. Keep titles and optional completion notes free of credentials, private prompt text, and unnecessary details; todo-tui may sync them. If tracking is unavailable, report it briefly and continue the requested work.",
    ],
    parameters: Type.Object({
      action: StringEnum(["start", "status", "pause", "finish"] as const),
      id: Type.Optional(Type.String({ pattern: "^[0-9a-f]{32}$", description: "Stable todo-tui task ID" })),
      title: Type.Optional(Type.String({ maxLength: 120, description: "Concise task title when starting without an existing ID" })),
      note: Type.Optional(Type.String({ maxLength: 240, description: "Optional completion note, only for finish" })),
    }, { additionalProperties: false }),
    executionMode: "sequential",
    annotations: { destructiveHint: false, openWorldHint: false },
    async execute(_id, params, signal, _onUpdate, ctx) {
      try {
        const active = await pins(ctx, signal);
        const respond = (text: string, ids: string[]) => ({ content: [{ type: "text" as const, text }], details: { ids } });
        if (params.action === "status") {
          const shown = active.slice(0, 12);
          const lines = shown.map((task) => `${task.id}: ${clean(task.title)}`);
          if (active.length > shown.length) lines.push(`+${active.length - shown.length} more`);
          return respond(lines.join("\n") || "No pinned items for this Pi agent.", shown.map((task) => task.id));
        }
        let id = params.id;
        if (id && !TASK_ID.test(id)) throw new Error("Use a stable task ID");
        if (params.action === "start") {
          const title = clean(params.title ?? "");
          if (!id && !title) throw new Error("Provide an existing task ID or a short new title");
          if (!id) {
            const project = clean(basename(ctx.cwd), 32);
            const taskTitle = project ? `${project}: ${title}` : title;
            id = active.find((task) => task.title === taskTitle)?.id;
            if (!id) {
              const task = await cli<Task>(["add", "--", taskTitle], signal);
              id = task.id;
              if (!TASK_ID.test(id)) throw new Error("Unexpected task ID from todo-tui");
            }
          }
          let task: Task;
          try {
            task = await cli<Task>(["pin", id, `--agent=${identity(ctx)}`, `--avatar=${avatar}`], signal);
          } catch (error) {
            throw new Error(`Could not pin ${id}: ${clean(String(error), 160)}. Retry start with that ID.`);
          }
          focusId = task.id;
          await refresh(ctx);
          return respond(`Pinned ${task.id}: ${clean(task.title)}`, [task.id]);
        }
        id ??= focusId ?? (active.length === 1 ? active[0].id : undefined);
        if (!id || !active.some((task) => task.id === id)) throw new Error("Provide the ID of a task pinned by this Pi agent");
        if (params.action === "pause") {
          await cli<Task>(["unpin", id, `--agent=${identity(ctx)}`], signal);
        } else {
          const task = await cli<Task>(["show", id], signal);
          if (task.pins.some((pin) => pin.agent !== identity(ctx))) throw new Error("Another agent also pinned this task. Pause instead of completing their shared task.");
          const note = clean(params.note ?? "", 240);
          await cli<Task>(["complete", id, ...(note ? [`--note=${note}`] : [])], signal);
        }
        if (focusId === id) focusId = undefined;
        await refresh(ctx);
        return respond(`${params.action === "pause" ? "Unpinned" : "Completed"} ${id}`, [id]);
      } catch (error) {
        return { content: [{ type: "text" as const, text: clean(String(error), 300) }], details: { ids: [] as string[] }, isError: true };
      }
    },
  });
}
