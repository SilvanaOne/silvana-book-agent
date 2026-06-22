import { PageSkeleton } from "@/app/components/PageSkeleton";

export default function Loading() {
  return <PageSkeleton heading="Rebalance builder" rows={5} />;
}
