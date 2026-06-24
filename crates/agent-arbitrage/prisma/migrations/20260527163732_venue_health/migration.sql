-- CreateTable
CREATE TABLE "VenueHealth" (
    "venueId" TEXT NOT NULL,
    "lastSuccessAt" TIMESTAMP(3),
    "lastErrorAt" TIMESTAMP(3),
    "lastError" TEXT,
    "successStreak" INTEGER NOT NULL DEFAULT 0,
    "errorStreak" INTEGER NOT NULL DEFAULT 0,
    "updatedAt" TIMESTAMP(3) NOT NULL,

    CONSTRAINT "VenueHealth_pkey" PRIMARY KEY ("venueId")
);
