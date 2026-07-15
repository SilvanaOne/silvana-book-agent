import { PageSkeleton } from "@/app/components/PageSkeleton";

export default function Loading() {
  return <PageSkeleton heading="Audit log" rows={10} />;
}
