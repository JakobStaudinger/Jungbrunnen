use assign_resources::assign_resources;
use embassy_rp::peripherals;

assign_resources! {
  led: LedPeripherals {
    pio: PIO1,
    red_pin: PIN_4,
    red_slice: PWM_SLICE2,
    green_pin: PIN_6,
    green_slice: PWM_SLICE3,
    blue_pin: PIN_2,
    blue_slice: PWM_SLICE1,
    dma_pwm_red_a: DMA_CH0,
    dma_pwm_red_b: DMA_CH1,
    dma_pwm_green_a: DMA_CH2,
    dma_pwm_green_b: DMA_CH3,
    dma_pwm_blue_a: DMA_CH4,
    dma_pwm_blue_b: DMA_CH5,
    dma_pio_red: DMA_CH6,
    dma_pio_green: DMA_CH7,
    dma_pio_blue: DMA_CH8,
  }
}
