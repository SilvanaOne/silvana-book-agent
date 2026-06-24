'use client';

import { useEffect } from 'react';
import { useRouter } from 'next/navigation';

/** Keys management moved to Silvana Vault — redirect legacy /keys bookmark. */
export default function KeysRedirectPage() {
  const router = useRouter();
  useEffect(() => {
    router.replace('/balances');
  }, [router]);
  return null;
}
