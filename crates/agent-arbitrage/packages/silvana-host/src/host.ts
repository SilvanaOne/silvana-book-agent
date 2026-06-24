/**
 * Silvana host runtime — foundation package.
 *
 * Wraps `@silvana-one/orderbook` SDK + cloud-agent sidecar handshake.
 * All Canton-side operations across the stack (open core, proprietary executor,
 * proprietary rebalancer) go through this layer.
 *
 * See md_docs/14-silvana-host-runtime.md for full architecture reference.
 *
 * Sprint 0 status: skeleton with `standalone-dev` mode only. Production mode
 * (real `cloud-agent` onboarding + gRPC clients) lands in Sprint 3 when Silvana
 * onboarding is unblocked.
 */

export type SilvanaHostMode = 'production' | 'standalone-dev';

export interface SilvanaHostConfig {
  readonly mode: SilvanaHostMode;
  readonly envPath?: string;
  readonly rpcUrl?: string;
}

export type HealthStatus = 'healthy' | 'degraded' | 'down';
export type HealthHandler = (status: HealthStatus) => void;
export type Unsubscribe = () => void;

export interface SilvanaHost {
  readonly mode: SilvanaHostMode;
  readonly partyId: string;
  readonly publicKey: string;

  start(): Promise<void>;
  stop(): Promise<void>;
  onHealthChange(handler: HealthHandler): Unsubscribe;
}

export async function createSilvanaHost(config: SilvanaHostConfig): Promise<SilvanaHost> {
  if (config.mode === 'standalone-dev') {
    return createStandaloneDevHost(config);
  }
  throw new Error(
    'Silvana production mode not yet implemented (Sprint 3 deliverable). ' +
      'Set mode: "standalone-dev" for Sprint 0/1 CEX-only development.',
  );
}

function createStandaloneDevHost(_config: SilvanaHostConfig): SilvanaHost {
  const handlers = new Set<HealthHandler>();
  return Object.freeze({
    mode: 'standalone-dev' as const,
    partyId: 'standalone-dev-party',
    publicKey: 'standalone-dev-pubkey',
    async start() {
      handlers.forEach((h) => h('healthy'));
    },
    async stop() {
      handlers.forEach((h) => h('down'));
    },
    onHealthChange(handler: HealthHandler): Unsubscribe {
      handlers.add(handler);
      return () => handlers.delete(handler);
    },
  });
}
