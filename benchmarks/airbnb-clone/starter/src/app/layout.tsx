import type { Metadata } from "next";
import "@/app/globals.css";
import Providers from "@/app/providers";

export const metadata: Metadata = {
  title: "Airbnb Clone",
  description: "Airbnb Clone — Sprint 1 starter",
};

export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <html lang="en">
      <body>
        <Providers>{children}</Providers>
      </body>
    </html>
  );
}
