import { PageSkeleton } from "@/app/components/PageSkeleton";

export default function Loading() {
  return <PageSkeleton heading="Execution monitor" rows={6} />;
}
