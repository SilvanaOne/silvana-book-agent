import { PrismaClient } from '@prisma/client';

export type { PrismaClient } from '@prisma/client';
export * from '@prisma/client';

let _client: PrismaClient | undefined;

/** Singleton Prisma client. Lazy-initialised on first call. */
export function getDb(): PrismaClient {
  if (!_client) {
    _client = new PrismaClient();
  }
  return _client;
}

export async function disconnectDb(): Promise<void> {
  if (_client) {
    await _client.$disconnect();
    _client = undefined;
  }
}
