<script lang="ts" module>
  import { type VariantProps, tv } from 'tailwind-variants';
  import { cn, type WithElementRef } from '$lib/utils';
  import type { HTMLAnchorAttributes, HTMLButtonAttributes } from 'svelte/elements';

  export const buttonVariants = tv({
    base: "inline-flex shrink-0 items-center justify-center whitespace-nowrap rounded-md border border-transparent bg-clip-padding text-sm font-medium outline-none transition-all select-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 active:not-aria-[haspopup]:translate-y-px disabled:pointer-events-none disabled:opacity-50",
    variants: {
      variant: {
        default: 'bg-primary text-primary-foreground hover:bg-primary/80',
        outline: 'border-border bg-background text-foreground shadow-xs hover:bg-muted',
        ghost: 'text-muted-foreground hover:bg-muted hover:text-foreground'
      },
      size: {
        default: 'h-8 gap-1.5 px-2.5',
        sm: 'h-7 gap-1 px-2 text-xs'
      }
    },
    defaultVariants: { variant: 'default', size: 'default' }
  });

  export type ButtonProps = WithElementRef<HTMLButtonAttributes> &
    WithElementRef<HTMLAnchorAttributes> & {
      variant?: VariantProps<typeof buttonVariants>['variant'];
      size?: VariantProps<typeof buttonVariants>['size'];
    };
</script>

<script lang="ts">
  let {
    class: className,
    variant = 'default',
    size = 'default',
    ref = $bindable(null),
    href = undefined,
    type = 'button',
    disabled,
    children,
    ...restProps
  }: ButtonProps = $props();
</script>

{#if href}
  <a
    bind:this={ref}
    data-slot="button"
    class={cn(buttonVariants({ variant, size }), className)}
    href={disabled ? undefined : href}
    aria-disabled={disabled}
    role={disabled ? 'link' : undefined}
    tabindex={disabled ? -1 : undefined}
    {...restProps}
  >
    {@render children?.()}
  </a>
{:else}
  <button
    bind:this={ref}
    data-slot="button"
    class={cn(buttonVariants({ variant, size }), className)}
    {type}
    {disabled}
    {...restProps}
  >
    {@render children?.()}
  </button>
{/if}
