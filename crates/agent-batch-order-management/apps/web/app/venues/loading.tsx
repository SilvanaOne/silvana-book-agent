import { PageSkeleton } from "@/app/components/PageSkeleton";

export default function Loading() {
  return <PageSkeleton heading="Venues" rows={4} />;
}
