import {
  OrderStatus,
  OrderUpdate_EventType,
  SettlementUpdate_EventType,

  type OrderUpdate,

} from "@silvana-one/orderbook";

/** JSON-снимок сообщения стрима (bigint → string). */
export function jsonFromStreamMessage(value: unknown): Record<string, unknown> {
  const json = JSON.stringify(value, (_, v: unknown) => {


    if (typeof v === "bigint") return v.toString();


    return v;



  });


  return JSON.parse(json) as Record<string, unknown>;


}




export function buildOrderStreamEventKey(silvanaOrderId: string, protoVersion: bigint, eventType: OrderUpdate_EventType): string {






  return `ob:stream:order:${silvanaOrderId}:ver:${protoVersion.toString()}:et:${eventType}`;


}




export function orderUpdateEventLabel(eventType: OrderUpdate_EventType): string {






  switch (eventType) {
    case OrderUpdate_EventType.UNSPECIFIED:


      return "silvana_order_update_unspecified";


    case OrderUpdate_EventType.CREATED:


      return "silvana_order_update_created";


    case OrderUpdate_EventType.UPDATED:


      return "silvana_order_update_updated";


    case OrderUpdate_EventType.FILLED:


      return "silvana_order_update_filled";


    case OrderUpdate_EventType.PARTIALLY_FILLED:


      return "silvana_order_update_partial";


    case OrderUpdate_EventType.CANCELLED:


      return "silvana_order_update_cancelled";


    case OrderUpdate_EventType.EXPIRED:


      return "silvana_order_update_expired";





    default:


      return `silvana_order_update_et_${eventType}`;





  }



}




export function settlementStreamEventLabel(eventType: SettlementUpdate_EventType): string {






  switch (eventType) {


    case SettlementUpdate_EventType.UNSPECIFIED:


      return "silvana_settlement_unspecified";





    case SettlementUpdate_EventType.PROPOSAL_CREATED:


      return "silvana_settlement_proposal_created";





    case SettlementUpdate_EventType.STATUS_CHANGED:


      return "silvana_settlement_status_changed";





    case SettlementUpdate_EventType.SETTLED:


      return "silvana_settlement_settled";





    case SettlementUpdate_EventType.FAILED:


      return "silvana_settlement_failed";





    case SettlementUpdate_EventType.CANCELLED:


      return "silvana_settlement_cancelled";





    default:


      return `silvana_settlement_et_${eventType}`;





  }



}




export function buildSettlementStreamEventKey(proposalId: string, eventType: SettlementUpdate_EventType): string {






  return `ob:stream:settlement:${proposalId}:et:${eventType}`;



}




/**
 * Снимок `order.status`, если указан на канале; иначе минимальный маппинг по `eventType`.
 */


export function resolveOrderBookStatusHint(u: OrderUpdate): OrderStatus {






  if (u.order?.status !== undefined && u.order.status !== OrderStatus.UNSPECIFIED) {


    return u.order.status;





  }





  switch (u.eventType) {
    case OrderUpdate_EventType.FILLED:


      return OrderStatus.FILLED;


    case OrderUpdate_EventType.PARTIALLY_FILLED:


      return OrderStatus.PARTIAL;


    case OrderUpdate_EventType.CANCELLED:


      return OrderStatus.CANCELLED;


    case OrderUpdate_EventType.EXPIRED:


      return OrderStatus.EXPIRED;


    case OrderUpdate_EventType.CREATED:


    case OrderUpdate_EventType.UPDATED:


      return OrderStatus.ACTIVE;


    default:


      return OrderStatus.UNSPECIFIED;



  }



}
