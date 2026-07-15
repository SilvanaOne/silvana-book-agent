import type { Metadata } from "next";

import { Inter } from "next/font/google";

import "./globals.css";

import { AppNav } from "./components/AppNav";

import { AppFooter } from "./components/AppFooter";

const inter = Inter({
  subsets: ["latin", "cyrillic"],


  weight: ["400", "500", "600", "700"],


  variable: "--font-inter",


  display: "swap",

});

export const metadata: Metadata = {
  title: "Batch Ops · Orderbook Agent",


  description:


    "Operator console: portfolio, rebalance, execution monitoring, venue audit (Silvana Book aesthetic).",

};

export default function RootLayout(props: Readonly<{ children: React.ReactNode }>) {


  return (


    <html lang="en" className={`${inter.variable} scroll-smooth`}>


      <body>


        <div className="app-root">


          <AppNav />


          {props.children}


          <AppFooter />

        </div>

      </body>

    </html>

  );


}
