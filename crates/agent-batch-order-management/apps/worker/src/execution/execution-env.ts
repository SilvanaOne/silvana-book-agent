export function executionRouterEnabledFromEnv(env: NodeJS.ProcessEnv = process.env): boolean {
  const v = env.EXECUTION_ROUTER_ENABLED?.trim().toLowerCase();
  return v === "1" || v === "true" || v === "yes";
}
