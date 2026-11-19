import { forwardRef, type ButtonHTMLAttributes } from "react";
import { clsx } from "clsx";

type ButtonProps = ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: "primary" | "outline" | "ghost" | "icon";
};

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(
  ({ className, variant = "outline", type = "button", ...props }, ref) => (
    <button ref={ref} type={type} className={clsx("button", `button-${variant}`, className)} {...props} />
  ),
);
Button.displayName = "Button";
