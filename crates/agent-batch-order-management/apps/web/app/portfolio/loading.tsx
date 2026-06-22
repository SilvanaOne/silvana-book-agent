import { PageSkeleton } from "@/app/components/PageSkeleton";

export default function Loading() {
  return <PageSkeleton heading="Portfolio overview" rows={8} />;
}
