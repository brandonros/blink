/* Non-waiting snapshots after masked/unmasked precision exceptions. */
static long call(long n,long a,long b,long c){long r;__asm__ volatile("syscall":"=a"(r):"a"(n),"D"(a),"S"(b),"d"(c):"rcx","r11","memory");return r;}
struct extended {unsigned long low;unsigned short high;} __attribute__((packed));
static unsigned char state[512] __attribute__((aligned(16)));
static unsigned char memory[16],record[64];
#define EXEC(instruction) __asm__ volatile("fninit; fldz; fldz; fldz; fldz; fldz; fldz; fldz; fldz; fninit; fldcw %3; fldt %1; fldt %2; " instruction "; fxsave64 %0; fninit":"=m"(state):"m"(y),"m"(x),"m"(control),"m"(memory):"st","st(1)","memory")
__attribute__((noreturn,force_align_arg_pointer)) void _start(void){
 for(unsigned op=0;op<9;op++)for(unsigned mode=0;mode<12;mode++)for(unsigned unmasked=0;unmasked<2;unmasked++){
  unsigned short control=0x7f|((mode<4?0:mode<8?2:3)<<8)|((mode&3)<<10);
  if(unmasked)control&=~32;
  struct extended x={0x8000000000000000ul,0x3fff},y={0xc000000000000000ul,0x4000};
  if(op==0)y=(struct extended){0x8000000000000000ul,0x3fb9};
  if(op==1)x=y=(struct extended){0x8ccccccccccccccdul,0x3fff};
  if(op==3)x=(struct extended){0x8000000000000000ul,0x4000};
  if(op==4||op==5||op==7)x=(struct extended){0xc000000000000000ul,0x3fff};
  if(op==6)x=(struct extended){0xaaaaaaaaaaaaaaabul,0x3ffd};
  for(int n=0;n<16;n++)memory[n]=0xa5;
  switch(op){
   case 0:EXEC("fadd %%st(1),%%st");break;
   case 1:EXEC("fmul %%st(1),%%st");break;
   case 2:EXEC("fdiv %%st(1),%%st");break;
   case 3:EXEC("fsqrt");break;
   case 4:EXEC("frndint");break;
   case 5:EXEC("fistpl %4");break;
   case 6:EXEC("fstps %4");break;
   case 7:EXEC("fbstp %4");break;
   case 8:EXEC("fdivrp %%st,%%st(1)");break;
  }
  for(int n=0;n<64;n++)record[n]=0;
  for(int n=0;n<10;n++){record[n]=state[32+n];record[10+n]=state[48+n];}
  for(int n=0;n<16;n++)record[20+n]=memory[n];
  record[36]=state[2];record[37]=state[3];record[38]=state[4];
  record[40]=op;record[41]=mode;record[42]=unmasked;
  if(call(1,1,(long)record,64)!=64){call(231,1,0,0);__builtin_unreachable();}
 }
 call(231,0,0,0);__builtin_unreachable();
}
