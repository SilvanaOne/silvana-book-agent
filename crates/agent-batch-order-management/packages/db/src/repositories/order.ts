import { Decimal } from "@prisma/client/runtime/library";
import { prisma } from "../client.js";

export async function createDraftOrdersInBatch(
  batchId: string,

  planned: ReadonlyArray<{
    side: string;
    type: string;

    market: string;
    venue?: string;
    execProfile?: string | null;
    price?: Decimal | string | null;
    qty: Decimal | string;
    clientOrderRef?: string | null;
    silvanaOrderId?: string | null;
    status?: string;
  }>,
  initialStatus = "pending_submit",
): Promise<Array<{ id: string }>> {

  return prisma.$transaction(async (tx) => {
    const rows: Array<{ id: string }> = [];
    for (const p of planned) {
      const qtyDec = typeof p.qty === "string" ? new Decimal(p.qty) : p.qty;
      const priceDec =
        p.price === undefined || p.price === null
          ? undefined
          : typeof p.price === "string"

            ? new Decimal(p.price)
            : p.price;

      rows.push(
        await tx.order.create({
          data: {
            orderBatchId: batchId,
            side: p.side,

            type: p.type,

            market: p.market,

            ...(p.venue !== undefined ? { venue: p.venue } : {}),
            ...(typeof p.execProfile === "string" && p.execProfile.length > 0 ? { execProfile: p.execProfile } : {}),

            price: priceDec,

            qty: qtyDec,

            silvanaOrderId: p.silvanaOrderId ?? undefined,

            clientOrderRef: p.clientOrderRef ?? undefined,

            status: p.status ?? initialStatus,

          },
          select: { id: true },
        }),

      );


    }


    await tx.orderBatch.update({
      where: { id: batchId },
      data: { totalOrders: rows.length },

    });


    return rows;


  });

}

export async function findOrderBySilvanaOrderId(silvanaOrderId: string) {
  return prisma.order.findUnique({ where: { silvanaOrderId } });
}

export async function findOrderBySilvanaOrderIdWithBatch(silvanaOrderId: string) {
  return prisma.order.findUnique({
    where: { silvanaOrderId },
    select: {
      id: true,
      orderBatchId: true,
      silvanaOrderId: true,
      orderBatch: { select: { rebalanceJobId: true } },
    },
  });
}


export async function updateOrderSilvanaBinding(params: Readonly<{ orderId: string; silvanaOrderId: string; status?: string }>) {


  return prisma.order.update({
    where: { id: params.orderId },

    data: {
      silvanaOrderId: params.silvanaOrderId,
      venueOrderRef: params.silvanaOrderId,
      venue: "silvana",
      ...(params.status !== undefined ? { status: params.status } : {}),

    },

    select: { id: true, silvanaOrderId: true, status: true },

  });

}


export async function findOrderWithBatch(orderId: string) {


  return prisma.order.findUnique({
    where: { id: orderId },

    include: {

      orderBatch: {
        select: { id: true, rebalanceJobId: true, status: true },
      },

    },

  });


}

export async function updateOrderPriceQty(orderId: string, params: Readonly<{ price: string | null; qty: string }>) {


  return prisma.order.update({
    where: { id: orderId },
    data: {
      price: params.price === null ? null : new Decimal(params.price),
      qty: new Decimal(params.qty),

    },

    select: { id: true, status: true, price: true, qty: true },

  });


}

export async function listOrdersByBatch(batchId: string) {


  return prisma.order.findMany({
    where: { orderBatchId: batchId },
    orderBy: { createdAt: "asc" },

  });

}

export async function listPendingSubmitOrdersByBatch(batchId: string) {

  return prisma.order.findMany({
    where: { orderBatchId: batchId, status: "pending_submit" },
    orderBy: { createdAt: "asc" },

  });


}

export async function updateOrderExecutionState(
  orderId: string,
  data: Readonly<{
    silvanaOrderId?: string | null;
    venueOrderRef?: string | null;
    status?: string;
    clientOrderRef?: string | null;
  }>,
) {
  return prisma.order.update({
    where: { id: orderId },
    data: {
      ...(data.silvanaOrderId !== undefined ? { silvanaOrderId: data.silvanaOrderId || null } : {}),
      ...(data.venueOrderRef !== undefined ? { venueOrderRef: data.venueOrderRef || null } : {}),
      ...(data.status !== undefined ? { status: data.status } : {}),
      ...(data.clientOrderRef !== undefined ? { clientOrderRef: data.clientOrderRef } : {}),
    },

    select: { id: true, status: true, silvanaOrderId: true },

  });

}
