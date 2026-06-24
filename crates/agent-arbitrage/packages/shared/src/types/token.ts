export type ChainId =
  | 'canton'
  | 'ethereum'
  | 'arbitrum'
  | 'bsc'
  | 'solana'
  | 'cex';

export interface Token {
  readonly symbol: string;
  readonly displayName: string;
  readonly decimals: number;
  readonly chainId: ChainId;
  readonly contractAddress?: string;
}

export const createToken = (
  symbol: string,
  displayName: string,
  decimals: number,
  chainId: ChainId,
  contractAddress?: string,
): Token =>
  Object.freeze({
    symbol,
    displayName,
    decimals,
    chainId,
    ...(contractAddress !== undefined ? { contractAddress } : {}),
  });
