import * as React from "react"
import { Dialog as DialogPrimitive } from "@base-ui/react/dialog"
import { XIcon } from "lucide-react"

import { t } from "@/lib/i18n"
import { cn } from "@/lib/utils"

/**
 * Every class the dialog parts use, in one place so a test can check that each one
 * exists in the Regulus theme (an unknown colour class compiles to nothing and the
 * panel turns transparent).
 *
 * Layout: the viewport covers the window and scrolls when even the smallest popup
 * does not fit; the popup is never taller than the viewport, and its body scrolls
 * between a sticky header and a sticky footer, so the title and the actions stay
 * on screen in a small window. The header and footer pull themselves into the
 * body's vertical padding (`-mt-6` / `-mb-6`) and stick at its outer edge
 * (`-top-6` / `-bottom-6`: a sticky box stops at the scroller's padding, not at
 * its border), so they look the same stuck or not and nothing shows past them;
 * that is why the header goes first and the footer last.
 */
export const dialogClasses = {
  backdrop:
    "fixed inset-0 z-50 bg-overlay transition-opacity duration-(--rg-duration-normal) ease-(--rg-ease-standard) data-starting-style:opacity-0 data-ending-style:opacity-0",
  viewport:
    "fixed inset-0 z-50 flex justify-center overflow-y-auto overscroll-contain p-4",
  popup:
    "relative my-auto flex max-h-[max(100%,20rem)] w-full max-w-lg flex-col overflow-hidden rounded-[var(--rg-radius-dialog)] border border-border bg-surface text-foreground shadow-[var(--rg-shadow-overlay),var(--rg-shadow-rim)] outline-none transition-opacity duration-(--rg-duration-normal) ease-(--rg-ease-standard) data-starting-style:opacity-0 data-ending-style:opacity-0",
  body:
    "grid min-h-0 gap-4 overflow-y-auto overscroll-contain px-6 py-6",
  close:
    "absolute top-4 right-4 z-20 flex size-7 items-center justify-center rounded-md text-muted-foreground transition-colors outline-none hover:bg-hover hover:text-foreground focus-visible:ring-3 focus-visible:ring-ring/50",
  header:
    "sticky -top-6 z-10 -mt-6 flex flex-col gap-1.5 bg-surface pt-6 pr-8 pb-2 text-left",
  footer:
    "sticky -bottom-6 z-10 -mb-6 flex flex-col-reverse gap-2 bg-surface pt-2 pb-6 sm:flex-row sm:justify-end",
  title: "text-lg leading-none font-semibold",
  description: "text-sm text-muted-foreground",
} as const

function Dialog({ ...props }: DialogPrimitive.Root.Props) {
  return <DialogPrimitive.Root data-slot="dialog" {...props} />
}

function DialogTrigger({ ...props }: DialogPrimitive.Trigger.Props) {
  return <DialogPrimitive.Trigger data-slot="dialog-trigger" {...props} />
}

function DialogPortal({ ...props }: DialogPrimitive.Portal.Props) {
  return <DialogPrimitive.Portal data-slot="dialog-portal" {...props} />
}

function DialogClose({ ...props }: DialogPrimitive.Close.Props) {
  return <DialogPrimitive.Close data-slot="dialog-close" {...props} />
}

function DialogOverlay({ className, ...props }: DialogPrimitive.Backdrop.Props) {
  return (
    <DialogPrimitive.Backdrop
      data-slot="dialog-overlay"
      className={cn(dialogClasses.backdrop, className)}
      {...props}
    />
  )
}

// Panel radius follows the dialog token (rg-radius-dialog), not the control one.
// `className` styles the popup (the opaque panel); the children go in its scrolling body.
function DialogContent({
  className,
  children,
  ...props
}: DialogPrimitive.Popup.Props) {
  return (
    <DialogPortal>
      <DialogOverlay />
      <DialogPrimitive.Viewport data-slot="dialog-viewport" className={dialogClasses.viewport}>
        <DialogPrimitive.Popup
          data-slot="dialog-content"
          className={cn(dialogClasses.popup, className)}
          {...props}
        >
          <div data-slot="dialog-body" className={dialogClasses.body}>
            {children}
          </div>
          <DialogPrimitive.Close
            data-slot="dialog-close-button"
            aria-label={t("dialog.close")}
            className={dialogClasses.close}
          >
            <XIcon size={16} strokeWidth={1.75} />
          </DialogPrimitive.Close>
        </DialogPrimitive.Popup>
      </DialogPrimitive.Viewport>
    </DialogPortal>
  )
}

function DialogHeader({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="dialog-header"
      className={cn(dialogClasses.header, className)}
      {...props}
    />
  )
}

function DialogFooter({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="dialog-footer"
      className={cn(dialogClasses.footer, className)}
      {...props}
    />
  )
}

function DialogTitle({
  className,
  ...props
}: DialogPrimitive.Title.Props) {
  return (
    <DialogPrimitive.Title
      data-slot="dialog-title"
      className={cn(dialogClasses.title, className)}
      {...props}
    />
  )
}

function DialogDescription({
  className,
  ...props
}: DialogPrimitive.Description.Props) {
  return (
    <DialogPrimitive.Description
      data-slot="dialog-description"
      className={cn(dialogClasses.description, className)}
      {...props}
    />
  )
}

export {
  Dialog,
  DialogTrigger,
  DialogPortal,
  DialogClose,
  DialogOverlay,
  DialogContent,
  DialogHeader,
  DialogFooter,
  DialogTitle,
  DialogDescription,
}
