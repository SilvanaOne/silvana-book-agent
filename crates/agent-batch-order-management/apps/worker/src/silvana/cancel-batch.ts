import type { OrderbookClient } from "@silvana-one/orderbook";
import { prisma, appendAuditLog, appendOrderEvent, listOrdersByBatch, updateOrderExecutionState, updateOrderBatch } from "@batch-order/db";
import { classifySilvanaError, parseSilvanaOrderId } from "@batch-order/silvana-client";

export async function cancelOrderBatchViaSilvana(params: Readonly<{ batchId: string; client: OrderbookClient }>): Promise<void> {


  const orders = await listOrdersByBatch(params.batchId);





  await updateOrderBatch(params.batchId, { status: "cancelling" });




  let hadError: unknown | null = null;





  for (const ord of orders) {


    if (ord.status === "cancelled" || ord.status === "filled" || ord.status === "expired") continue;



    if (!ord.silvanaOrderId) {

      await updateOrderExecutionState(ord.id, { status: "cancelled" });


      await prisma.orderBatch.update({


        where: { id: params.batchId },


        data: { cancelledOrders: { increment: 1 } },

      });


      continue;


    }




    try {


      const resp = await params.client.cancelOrder({


        orderId: parseSilvanaOrderId(ord.silvanaOrderId),


      });


      if (!resp.success) throw new Error(resp.message?.trim() ? resp.message : "cancelOrder unsuccessful");




      await updateOrderExecutionState(ord.id, { status: "cancelled" });





      await prisma.orderBatch.update({


        where: { id: params.batchId },




        data: { cancelledOrders: { increment: 1 } },


      });


      await appendOrderEvent({
        orderId: ord.id,
        eventType: "silvana_cancel",
        payload: JSON.parse(JSON.stringify({ silvanaOrderId: ord.silvanaOrderId, silvanaMessage: resp.message })),
      });


    } catch (e: unknown) {


      hadError = e;





      await appendAuditLog({




        actorId: "worker",



        action: "batch.cancel.failed",



        entityType: "order",

        entityId: ord.id,

        payload: JSON.parse(JSON.stringify(classifySilvanaError(e))),


      });

    }



  }



  await updateOrderBatch(params.batchId, { status: hadError ? "failed" : "cancelled" });




  if (hadError) throw hadError;



}
